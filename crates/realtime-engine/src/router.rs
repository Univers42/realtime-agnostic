/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   router.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:18 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:14:21 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Event router — the central fan-out loop of the engine.
//!
//! The router sits between the event bus and the gateway. It:
//! 1. Consumes events from the bus subscriber.
//! 2. Assigns a per-topic sequence number.
//! 3. Wraps the event in `Arc` for zero-copy fan-out.
//! 4. Queries the [`SubscriptionRegistry`] for matching connections.
//! 5. Sends a [`LocalDispatch`] per match into the dispatch channel.
//!
//! The gateway's fan-out task reads from the dispatch channel and
//! writes into each connection's individual send queue.

use std::sync::Arc;

use realtime_core::{
    ConnectionId, EventBusSubscriber,
    EventEnvelope, SubscriptionId,
};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::registry::SubscriptionRegistry;
use crate::sequence::SequenceGenerator;

/// Type alias for the dispatch callback (currently unused in favour of channels).
pub type DispatchCallback =
    Arc<dyn Fn(ConnectionId, SubscriptionId, Arc<EventEnvelope>) + Send + Sync>;

/// The event router: consumes bus events, evaluates subscriptions, dispatches matches.
///
/// ## Architecture
///
/// ```text
/// EventBusSubscriber ──► EventRouter ──► mpsc::channel ──► FanOut task
///                          │
///                 SubscriptionRegistry
/// ```
pub struct EventRouter {
    registry: Arc<SubscriptionRegistry>,
    sequence_gen: Arc<SequenceGenerator>,
    dispatch_tx: mpsc::Sender<LocalDispatch>,
}

/// A dispatch instruction destined for a local connection.
///
/// Contains the connection ID, subscription ID, and an `Arc`-wrapped
/// event (shared across all recipients for zero-copy).
#[derive(Debug, Clone)]
pub struct LocalDispatch {
    pub conn_id: ConnectionId,
    pub sub_id: SubscriptionId,
    pub event: Arc<EventEnvelope>,
}

impl EventRouter {
    /// Create a new router.
    ///
    /// # Arguments
    ///
    /// * `registry` — Shared subscription registry.
    /// * `sequence_gen` — Per-topic sequence generator.
    /// * `dispatch_tx` — Channel sender for dispatch instructions.
    pub fn new(
        registry: Arc<SubscriptionRegistry>,
        sequence_gen: Arc<SequenceGenerator>,
        dispatch_tx: mpsc::Sender<LocalDispatch>,
    ) -> Self {
        Self {
            registry,
            sequence_gen,
            dispatch_tx,
        }
    }

    /// Route a single event to all matching subscribers.
    ///
    /// 1. Assigns a monotonically increasing sequence number.
    /// 2. Wraps the event in `Arc` (one allocation, shared across N connections).
    /// 3. Queries the registry for matching subscriptions.
    /// 4. Sends a [`LocalDispatch`] for each match.
    ///
    /// # Returns
    ///
    /// The number of connections the event was dispatched to.
    pub async fn route_event(&self, mut event: EventEnvelope) -> usize {
        // Assign sequence number
        let seq = self.sequence_gen.next(event.topic.as_str());
        event.sequence = seq;

        // Wrap in Arc for zero-copy fan-out
        let event = Arc::new(event);

        // Look up matching subscriptions
        let matches = self.registry.lookup_matches(&event);
        let match_count = matches.len();

        if match_count == 0 {
            debug!(topic = %event.topic, "No matching subscriptions for event");
            return 0;
        }

        debug!(
            topic = %event.topic,
            event_id = %event.event_id,
            match_count,
            "Routing event to subscribers"
        );

        // Dispatch to each matching connection
        for (conn_id, sub_id, _gateway_node) in matches {
            let dispatch = LocalDispatch {
                conn_id,
                sub_id,
                event: Arc::clone(&event),
            };

            if let Err(e) = self.dispatch_tx.try_send(dispatch) {
                warn!(
                    conn_id = %conn_id,
                    "Dispatch channel full or closed: {}", e
                );
            }
        }

        match_count
    }

    /// Start the router loop, consuming events from a bus subscriber.
    ///
    /// This is a long-running task. It blocks on `subscriber.next_event()`
    /// in a loop, routes each event, then acks it on the bus.
    ///
    /// Call this from a `tokio::spawn` with a `Box<dyn EventBusSubscriber>`.
    pub async fn run_with_subscriber(
        &self,
        mut subscriber: Box<dyn EventBusSubscriber>,
    ) {
        info!("Event router started");

        while let Some(event) = subscriber.next_event().await {
            let _routed = self.route_event(event.clone()).await;

            // Ack the event on the bus
            if let Err(e) = subscriber.ack(&event.event_id).await {
                error!("Failed to ack event {}: {}", event.event_id, e);
            }
        }

        info!("Event router stopped (subscriber closed)");
    }

    /// Return the current sequence number for a topic (without incrementing).
    pub fn current_sequence(&self, topic: &str) -> u64 {
        self.sequence_gen.current(topic)
    }

    /// Return a reference to the underlying subscription registry.
    pub fn registry(&self) -> &Arc<SubscriptionRegistry> {
        &self.registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use realtime_core::{SubConfig, Subscription, SubscriptionId, TopicPath, TopicPattern};
    use smol_str::SmolStr;

    #[tokio::test]
    async fn test_route_event_to_single_subscriber() {
        let registry = Arc::new(SubscriptionRegistry::new());
        let seq_gen = Arc::new(SequenceGenerator::new());
        let (dispatch_tx, mut dispatch_rx) = mpsc::channel(1024);

        let router = EventRouter::new(
            Arc::clone(&registry),
            seq_gen,
            dispatch_tx,
        );

        // Subscribe
        let sub = Subscription {
            sub_id: SubscriptionId(SmolStr::new("sub-1")),
            conn_id: ConnectionId(1),
            topic: TopicPattern::parse("orders/created"),
            filter: None,
            config: SubConfig::default(),
        };
        registry.subscribe(sub, None);

        // Route event
        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from(r#"{"id": 1}"#),
        );
        let count = router.route_event(event).await;
        assert_eq!(count, 1);

        // Check dispatch
        let dispatch = dispatch_rx.recv().await.unwrap();
        assert_eq!(dispatch.conn_id, ConnectionId(1));
        assert_eq!(dispatch.event.sequence, 1);
    }

    #[tokio::test]
    async fn test_route_event_to_multiple_subscribers() {
        let registry = Arc::new(SubscriptionRegistry::new());
        let seq_gen = Arc::new(SequenceGenerator::new());
        let (dispatch_tx, mut dispatch_rx) = mpsc::channel(4096);

        let router = EventRouter::new(
            Arc::clone(&registry),
            seq_gen,
            dispatch_tx,
        );

        // Subscribe 100 connections
        for i in 0..100u64 {
            let sub = Subscription {
                sub_id: SubscriptionId(SmolStr::new(format!("sub-{i}"))),
                conn_id: ConnectionId(i),
                topic: TopicPattern::parse("broadcast"),
                filter: None,
                config: SubConfig::default(),
            };
            registry.subscribe(sub, None);
        }

        let event = EventEnvelope::new(
            TopicPath::new("broadcast"),
            "notify",
            Bytes::from("{}"),
        );
        let count = router.route_event(event).await;
        assert_eq!(count, 100);

        // Drain dispatch channel
        let mut received = 0;
        while let Ok(_dispatch) = dispatch_rx.try_recv() {
            received += 1;
        }
        assert_eq!(received, 100);
    }

    #[tokio::test]
    async fn test_no_matches() {
        let registry = Arc::new(SubscriptionRegistry::new());
        let seq_gen = Arc::new(SequenceGenerator::new());
        let (dispatch_tx, _dispatch_rx) = mpsc::channel(1024);

        let router = EventRouter::new(
            Arc::clone(&registry),
            seq_gen,
            dispatch_tx,
        );

        let event = EventEnvelope::new(
            TopicPath::new("no/subscribers"),
            "test",
            Bytes::from("{}"),
        );
        let count = router.route_event(event).await;
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_sequence_numbers_increment() {
        let registry = Arc::new(SubscriptionRegistry::new());
        let seq_gen = Arc::new(SequenceGenerator::new());
        let (dispatch_tx, mut dispatch_rx) = mpsc::channel(1024);

        let router = EventRouter::new(
            Arc::clone(&registry),
            seq_gen,
            dispatch_tx,
        );

        let sub = Subscription {
            sub_id: SubscriptionId(SmolStr::new("sub-1")),
            conn_id: ConnectionId(1),
            topic: TopicPattern::parse("orders/*"),
            filter: None,
            config: SubConfig::default(),
        };
        registry.subscribe(sub, None);

        for i in 1..=5 {
            let event = EventEnvelope::new(
                TopicPath::new("orders/created"),
                "created",
                Bytes::from("{}"),
            );
            router.route_event(event).await;

            let dispatch = dispatch_rx.recv().await.unwrap();
            assert_eq!(dispatch.event.sequence, i);
        }
    }
}

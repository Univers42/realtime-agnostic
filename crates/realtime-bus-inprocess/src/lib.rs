/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   lib.rs                                             :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:12:05 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! # realtime-bus-inprocess
//!
//! In-process event bus implementation using [`tokio::sync::broadcast`] channels.
//!
//! This is the simplest possible [`EventBus`] implementation:
//! zero external dependencies, zero serialization, zero network hops. Suitable for
//! single-node deployments and testing.
//!
//! ## Capacity
//!
//! The bus has a configurable capacity (default: 65,536 messages). When a subscriber
//! falls behind, it receives a `Lagged(n)` notification and skips ahead — no unbounded
//! memory growth, no blocking of publishers.
//!
//! ## Replacing with a Distributed Bus
//!
//! For multi-node deployments, implement [`EventBus`] for
//! Redis Streams, NATS JetStream, or Apache Kafka. The rest of the system works
//! unchanged — only this crate needs to be swapped.

use async_trait::async_trait;
use realtime_core::{
    EventBus, EventBusPublisher, EventBusSubscriber, EventEnvelope, EventId,
    PublishReceipt, RealtimeError, Result,
};
use tokio::sync::broadcast;
use tracing::{debug, info};

/// In-process event bus implementation using tokio broadcast channels.
///
/// All publishers and subscribers share a single `broadcast::Sender`.
/// No external dependencies — suitable for single-node deployment and testing.
pub struct InProcessBus {
    sender: broadcast::Sender<EventEnvelope>,
    _capacity: usize,
}

impl InProcessBus {
    /// Create a new in-process bus with the specified channel capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` — Maximum number of in-flight messages. When a slow subscriber
    ///   falls behind, it receives `Lagged(n)` and skips ahead automatically.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        info!(capacity, "In-process event bus created");
        Self { sender, _capacity: capacity }
    }

    /// Create with default capacity (65536 messages).
    pub fn default_capacity() -> Self {
        Self::new(65536)
    }
}

#[async_trait]
impl EventBus for InProcessBus {
    async fn publisher(&self) -> Result<Box<dyn EventBusPublisher>> {
        Ok(Box::new(InProcessPublisher {
            sender: self.sender.clone(),
        }))
    }

    async fn subscriber(&self, _topic_pattern: &str) -> Result<Box<dyn EventBusSubscriber>> {
        let receiver = self.sender.subscribe();
        Ok(Box::new(InProcessSubscriber { receiver }))
    }

    async fn health_check(&self) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        info!("In-process event bus shut down");
        Ok(())
    }
}

/// Publisher side of the in-process bus.
///
/// Clones of the `broadcast::Sender` are cheap — they share the same
/// internal ring buffer via `Arc`.
struct InProcessPublisher {
    sender: broadcast::Sender<EventEnvelope>,
}

#[async_trait]
impl EventBusPublisher for InProcessPublisher {
    async fn publish(&self, _topic: &str, event: &EventEnvelope) -> Result<PublishReceipt> {
        self.sender.send(event.clone()).map_err(|e| {
            RealtimeError::EventBusError(format!("Failed to publish: {}", e))
        })?;

        debug!(event_id = %event.event_id, topic = %event.topic, "Event published to in-process bus");

        Ok(PublishReceipt {
            event_id: event.event_id.clone(),
            sequence: 0, // Sequence assigned by router
            delivered_to_bus: true,
        })
    }

    async fn publish_batch(
        &self,
        events: &[(String, EventEnvelope)],
    ) -> Result<Vec<PublishReceipt>> {
        let mut receipts = Vec::with_capacity(events.len());
        for (topic, event) in events {
            receipts.push(self.publish(topic, event).await?);
        }
        Ok(receipts)
    }
}

/// Subscriber side of the in-process bus.
///
/// Each subscriber holds its own `broadcast::Receiver` cursor.
/// If it falls behind, `recv()` returns `Lagged(n)` which the
/// implementation logs and skips past.
struct InProcessSubscriber {
    receiver: broadcast::Receiver<EventEnvelope>,
}

#[async_trait]
impl EventBusSubscriber for InProcessSubscriber {
    async fn next_event(&mut self) -> Option<EventEnvelope> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("In-process subscriber lagged behind by {} messages", n);
                    // Continue receiving, skip the lagged messages
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return None;
                }
            }
        }
    }

    async fn ack(&self, _event_id: &EventId) -> Result<()> {
        // In-process bus doesn't require acknowledgment
        Ok(())
    }

    async fn nack(&self, _event_id: &EventId) -> Result<()> {
        // In-process bus doesn't support redelivery
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use realtime_core::TopicPath;

    #[tokio::test]
    async fn test_publish_and_subscribe() {
        let bus = InProcessBus::new(1024);

        let publisher = bus.publisher().await.unwrap();
        let mut subscriber = bus.subscriber("*").await.unwrap();

        let event = EventEnvelope::new(
            TopicPath::new("test/event"),
            "created",
            Bytes::from(r#"{"key":"value"}"#),
        );

        let receipt = publisher.publish("test/event", &event).await.unwrap();
        assert!(receipt.delivered_to_bus);

        let received = subscriber.next_event().await.unwrap();
        assert_eq!(received.topic, event.topic);
        assert_eq!(received.event_type, "created");
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let bus = InProcessBus::new(1024);

        let publisher = bus.publisher().await.unwrap();
        let mut sub1 = bus.subscriber("*").await.unwrap();
        let mut sub2 = bus.subscriber("*").await.unwrap();

        let event = EventEnvelope::new(
            TopicPath::new("test"),
            "test",
            Bytes::from("{}"),
        );

        publisher.publish("test", &event).await.unwrap();

        let r1 = sub1.next_event().await.unwrap();
        let r2 = sub2.next_event().await.unwrap();

        assert_eq!(r1.event_id, r2.event_id);
    }

    #[tokio::test]
    async fn test_batch_publish() {
        let bus = InProcessBus::new(1024);

        let publisher = bus.publisher().await.unwrap();
        let mut subscriber = bus.subscriber("*").await.unwrap();

        let events: Vec<(String, EventEnvelope)> = (0..5)
            .map(|i| {
                let e = EventEnvelope::new(
                    TopicPath::new(&format!("test/{i}")),
                    "created",
                    Bytes::from("{}"),
                );
                (format!("test/{i}"), e)
            })
            .collect();

        let receipts = publisher.publish_batch(&events).await.unwrap();
        assert_eq!(receipts.len(), 5);

        for _ in 0..5 {
            let received = subscriber.next_event().await.unwrap();
            assert_eq!(received.event_type, "created");
        }
    }

    #[tokio::test]
    async fn test_health_check() {
        let bus = InProcessBus::new(1024);
        assert!(bus.health_check().await.is_ok());
    }
}

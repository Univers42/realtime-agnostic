/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   registry.rs                                        :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:11 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Subscription registry — the core data structure mapping topics to connections.
//!
//! The registry maintains three concurrent indexes (all backed by [`DashMap`]
//! for lock-free reads):
//!
//! 1. **by_connection** — `ConnectionId → Vec<SubscriptionEntry>` (used on disconnect)
//! 2. **by_topic** — `pattern_string → Vec<(ConnectionId, SubscriptionId)>` (used for matching)
//! 3. **by_sub_id** — `(ConnectionId, sub_id) → SubscriptionEntry` (used for unsubscribe)
//!
//! Plus a [`FilterIndex`] that accelerates filter evaluation for
//! high-cardinality subscription sets using bitmaps.

use std::sync::Arc;

use dashmap::DashMap;
use realtime_core::{
    ConnectionId, EventEnvelope, Subscription, SubscriptionId, TopicPattern,
};
use tracing::{debug, info, warn};

use crate::filter_index::FilterIndex;

/// Entry for a single subscription in the registry.
///
/// Wraps the core [`Subscription`] with gateway routing information.
#[derive(Debug, Clone)]
pub struct SubscriptionEntry {
    /// The subscription configuration (topic, filter, config, etc.).
    pub subscription: Subscription,
    /// Gateway node hosting the connection (for future multi-node routing).
    pub gateway_node: Option<realtime_core::NodeId>,
}

/// The subscription registry: maps topics to connections with optional filters.
///
/// Thread-safe, lock-free reads via [`DashMap`] sharding. This is the
/// hot-path data structure — every incoming event queries it to determine
/// which connections should receive the event.
///
/// ## Performance characteristics
///
/// - **Read**: O(P) where P = number of registered patterns (typically small)
/// - **Subscribe**: O(1) amortized (DashMap insert)
/// - **Unsubscribe**: O(S) where S = subscriptions per connection
/// - **Remove connection**: O(S) — cleans all indexes in one pass
pub struct SubscriptionRegistry {
    /// conn_id → list of subscription entries (for disconnect cleanup).
    by_connection: DashMap<ConnectionId, Vec<SubscriptionEntry>>,

    /// topic_pattern string → list of (conn_id, sub_id) pairs.
    by_topic: DashMap<String, Vec<(ConnectionId, SubscriptionId)>>,

    /// (conn_id, sub_id_string) → SubscriptionEntry (for unsubscribe + filter lookup).
    by_sub_id: DashMap<(ConnectionId, String), SubscriptionEntry>,

    /// All known topic patterns (for matching incoming event topics).
    patterns: DashMap<String, TopicPattern>,

    /// Bitmap-based filter index for accelerated filter evaluation.
    filter_index: Arc<FilterIndex>,
}

impl SubscriptionRegistry {
    /// Create a new empty registry with default DashMap capacity.
    pub fn new() -> Self {
        Self {
            by_connection: DashMap::new(),
            by_topic: DashMap::new(),
            by_sub_id: DashMap::new(),
            patterns: DashMap::new(),
            filter_index: Arc::new(FilterIndex::new()),
        }
    }

    /// Register a new subscription, indexing it in all three maps.
    ///
    /// # Arguments
    ///
    /// * `sub` — The subscription to register.
    /// * `gateway_node` — Optional node ID for multi-node routing.
    pub fn subscribe(&self, sub: Subscription, gateway_node: Option<realtime_core::NodeId>) {
        let entry = SubscriptionEntry {
            subscription: sub.clone(),
            gateway_node,
        };

        // Index by connection
        self.by_connection
            .entry(sub.conn_id)
            .or_default()
            .push(entry.clone());

        // Index by topic pattern
        let pattern_key = sub.topic.as_str().to_string();
        self.by_topic
            .entry(pattern_key.clone())
            .or_default()
            .push((sub.conn_id, sub.sub_id.clone()));

        // Store the pattern
        self.patterns
            .entry(pattern_key)
            .or_insert_with(|| sub.topic.clone());

        // Index by sub_id
        self.by_sub_id
            .insert((sub.conn_id, sub.sub_id.0.to_string()), entry.clone());

        // Index in filter index (for bitmap evaluation)
        self.filter_index.add_subscription(&sub);

        debug!(
            conn_id = %sub.conn_id,
            sub_id = %sub.sub_id,
            topic = %sub.topic,
            "Subscription registered"
        );
    }

    /// Remove a specific subscription by connection + sub_id.
    ///
    /// Cleans all three indexes and the filter index. Returns `true`
    /// if the subscription existed and was removed.
    pub fn unsubscribe(&self, conn_id: ConnectionId, sub_id: &str) -> bool {
        let key = (conn_id, sub_id.to_string());

        if let Some((_, entry)) = self.by_sub_id.remove(&key) {
            // Remove from by_connection
            if let Some(mut subs) = self.by_connection.get_mut(&conn_id) {
                subs.retain(|e| e.subscription.sub_id.0.as_str() != sub_id);
            }

            // Remove from by_topic
            let pattern_key = entry.subscription.topic.as_str().to_string();
            if let Some(mut topic_subs) = self.by_topic.get_mut(&pattern_key) {
                topic_subs.retain(|(cid, sid)| !(*cid == conn_id && sid.0.as_str() == sub_id));
            }

            // Remove from filter index
            self.filter_index.remove_subscription(&entry.subscription);

            debug!(conn_id = %conn_id, sub_id = sub_id, "Subscription removed");
            true
        } else {
            warn!(conn_id = %conn_id, sub_id = sub_id, "Subscription not found for removal");
            false
        }
    }

    /// Remove **all** subscriptions for a connection (called on disconnect).
    ///
    /// Iterates once through the connection's subscriptions and removes
    /// each from every index, avoiding O(N²) cleanup.
    pub fn remove_connection(&self, conn_id: ConnectionId) {
        if let Some((_, subs)) = self.by_connection.remove(&conn_id) {
            for entry in &subs {
                let sub = &entry.subscription;
                let pattern_key = sub.topic.as_str().to_string();

                // Remove from by_topic
                if let Some(mut topic_subs) = self.by_topic.get_mut(&pattern_key) {
                    topic_subs.retain(|(cid, _)| *cid != conn_id);
                }

                // Remove from by_sub_id
                self.by_sub_id.remove(&(conn_id, sub.sub_id.0.to_string()));

                // Remove from filter index
                self.filter_index.remove_subscription(sub);
            }
            info!(conn_id = %conn_id, count = subs.len(), "All subscriptions removed for connection");
        }
    }

    /// Look up all matching connections for an event, applying server-side filters.
    ///
    /// This is the **hot path** of the engine. For each registered topic
    /// pattern, if it matches the event's topic, every subscription on
    /// that pattern is checked against its filter (if any).
    ///
    /// # Returns
    ///
    /// A vector of `(ConnectionId, SubscriptionId, Option<NodeId>)` for
    /// every subscription that should receive this event.
    pub fn lookup_matches(
        &self,
        event: &EventEnvelope,
    ) -> Vec<(ConnectionId, SubscriptionId, Option<realtime_core::NodeId>)> {
        let mut matches = Vec::new();

        // Check each registered pattern against the event topic
        for pattern_ref in self.patterns.iter() {
            let pattern = pattern_ref.value();
            if !pattern.matches(&event.topic) {
                continue;
            }

            let pattern_key = pattern_ref.key();
            if let Some(topic_subs) = self.by_topic.get(pattern_key.as_str()) {
                for (conn_id, sub_id) in topic_subs.iter() {
                    let key = (*conn_id, sub_id.0.to_string());
                    if let Some(entry) = self.by_sub_id.get(&key) {
                        // Apply filter if present
                        if let Some(ref filter) = entry.subscription.filter {
                            let field_getter = |field: &realtime_core::filter::FieldPath| {
                                realtime_core::filter::envelope_field_getter(event, field)
                            };
                            if !filter.evaluate(&field_getter) {
                                continue;
                            }
                        }
                        matches.push((*conn_id, sub_id.clone(), entry.gateway_node));
                    }
                }
            }
        }

        matches
    }

    /// Optimized lookup using the bitmap filter index.
    ///
    /// For high-cardinality scenarios (10k+ subscriptions), the bitmap
    /// approach can be faster than iterating all subscriptions because
    /// it evaluates all filters in a single pass using bitwise operations.
    pub fn lookup_matches_bitmap(
        &self,
        event: &EventEnvelope,
    ) -> Vec<(ConnectionId, SubscriptionId, Option<realtime_core::NodeId>)> {
        // Use bitmap index for fast evaluation
        let bitmap = self.filter_index.evaluate(event);

        let mut matches = Vec::new();
        for conn_id_raw in bitmap.iter() {
            let conn_id = ConnectionId(conn_id_raw as u64);
            if let Some(subs) = self.by_connection.get(&conn_id) {
                for entry in subs.iter() {
                    if entry.subscription.topic.matches(&event.topic) {
                        matches.push((
                            conn_id,
                            entry.subscription.sub_id.clone(),
                            entry.gateway_node,
                        ));
                    }
                }
            }
        }

        matches
    }

    /// Return the total count of active subscriptions across all connections.
    pub fn subscription_count(&self) -> usize {
        self.by_sub_id.len()
    }

    /// Return the number of connections that have at least one subscription.
    pub fn connection_count(&self) -> usize {
        self.by_connection.len()
    }

    /// Return a clone of all subscriptions for a given connection.
    pub fn get_connection_subscriptions(&self, conn_id: ConnectionId) -> Vec<SubscriptionEntry> {
        self.by_connection
            .get(&conn_id)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Return the current sequence for a topic (placeholder — managed by `SequenceGenerator`).
    pub fn get_topic_sequence(&self, _topic: &str) -> u64 {
        0 // Sequence is managed by SequenceGenerator, not registry
    }
}

impl Default for SubscriptionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use realtime_core::{FilterExpr, SubConfig, TopicPath};
    use smol_str::SmolStr;

    fn make_sub(conn_id: u64, sub_id: &str, topic: &str, filter: Option<FilterExpr>) -> Subscription {
        Subscription {
            sub_id: SubscriptionId(SmolStr::new(sub_id)),
            conn_id: ConnectionId(conn_id),
            topic: TopicPattern::parse(topic),
            filter,
            config: SubConfig::default(),
        }
    }

    #[test]
    fn test_subscribe_and_lookup() {
        let registry = SubscriptionRegistry::new();

        let sub = make_sub(1, "sub-1", "orders/created", None);
        registry.subscribe(sub, None);

        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );

        let matches = registry.lookup_matches(&event);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, ConnectionId(1));
    }

    #[test]
    fn test_glob_pattern_matching() {
        let registry = SubscriptionRegistry::new();

        let sub = make_sub(1, "sub-1", "orders/*", None);
        registry.subscribe(sub, None);

        let event1 = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let event2 = EventEnvelope::new(
            TopicPath::new("users/created"),
            "created",
            Bytes::from("{}"),
        );

        assert_eq!(registry.lookup_matches(&event1).len(), 1);
        assert_eq!(registry.lookup_matches(&event2).len(), 0);
    }

    #[test]
    fn test_filter_matching() {
        let registry = SubscriptionRegistry::new();

        let filter = FilterExpr::Eq(
            realtime_core::filter::FieldPath::new("event_type"),
            realtime_core::filter::FilterValue::String("created".to_string()),
        );
        let sub = make_sub(1, "sub-1", "orders/*", Some(filter));
        registry.subscribe(sub, None);

        let event_match = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let event_no_match = EventEnvelope::new(
            TopicPath::new("orders/deleted"),
            "deleted",
            Bytes::from("{}"),
        );

        assert_eq!(registry.lookup_matches(&event_match).len(), 1);
        assert_eq!(registry.lookup_matches(&event_no_match).len(), 0);
    }

    #[test]
    fn test_unsubscribe() {
        let registry = SubscriptionRegistry::new();

        let sub = make_sub(1, "sub-1", "orders/created", None);
        registry.subscribe(sub, None);
        assert_eq!(registry.subscription_count(), 1);

        registry.unsubscribe(ConnectionId(1), "sub-1");
        assert_eq!(registry.subscription_count(), 0);
    }

    #[test]
    fn test_remove_connection() {
        let registry = SubscriptionRegistry::new();

        let sub1 = make_sub(1, "sub-1", "orders/created", None);
        let sub2 = make_sub(1, "sub-2", "users/updated", None);
        registry.subscribe(sub1, None);
        registry.subscribe(sub2, None);
        assert_eq!(registry.subscription_count(), 2);

        registry.remove_connection(ConnectionId(1));
        assert_eq!(registry.subscription_count(), 0);
        assert_eq!(registry.connection_count(), 0);
    }

    #[test]
    fn test_multiple_connections_same_topic() {
        let registry = SubscriptionRegistry::new();

        for i in 0..100 {
            let sub = make_sub(i, &format!("sub-{i}"), "broadcast", None);
            registry.subscribe(sub, None);
        }

        let event = EventEnvelope::new(
            TopicPath::new("broadcast"),
            "notify",
            Bytes::from("{}"),
        );

        let matches = registry.lookup_matches(&event);
        assert_eq!(matches.len(), 100);
    }
}

//! Subscription registry — the core data structure mapping topics to connections.
//!
//! The registry maintains three concurrent indexes (all backed by [`DashMap`]
//! for lock-free reads):
//!
//! 1. **`by_connection`** — `ConnectionId → Vec<SubscriptionEntry>` (used on disconnect)
//! 2. **`by_topic`** — `pattern_string → Vec<(ConnectionId, SubscriptionId)>` (used for matching)
//! 3. **`by_sub_id`** — `(ConnectionId, sub_id) → SubscriptionEntry` (used for unsubscribe)
//!
//! Plus a [`FilterIndex`] that accelerates filter evaluation for
//! high-cardinality subscription sets using bitmaps.

mod counters;
mod lookup;
mod unsubscribe;

use std::sync::Arc;

use dashmap::DashMap;
use realtime_core::{ConnectionId, Subscription, SubscriptionId, TopicPattern};
use tracing::debug;

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
pub struct SubscriptionRegistry {
    by_connection: DashMap<ConnectionId, Vec<SubscriptionEntry>>,
    by_topic: DashMap<String, Vec<(ConnectionId, SubscriptionId)>>,
    by_sub_id: DashMap<(ConnectionId, String), SubscriptionEntry>,
    patterns: DashMap<String, TopicPattern>,
    filter_index: Arc<FilterIndex>,
}

impl SubscriptionRegistry {
    /// Create a new empty registry with default `DashMap` capacity.
    #[must_use]
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
    #[allow(clippy::needless_pass_by_value)]
    pub fn subscribe(&self, sub: Subscription, gateway_node: Option<realtime_core::NodeId>) {
        let entry = SubscriptionEntry {
            subscription: sub.clone(),
            gateway_node,
        };
        self.by_connection
            .entry(sub.conn_id)
            .or_default()
            .push(entry.clone());
        let pattern_key = sub.topic.as_str().to_string();
        self.by_topic
            .entry(pattern_key.clone())
            .or_default()
            .push((sub.conn_id, sub.sub_id.clone()));
        self.patterns
            .entry(pattern_key)
            .or_insert_with(|| sub.topic.clone());
        self.by_sub_id
            .insert((sub.conn_id, sub.sub_id.0.to_string()), entry);
        self.filter_index.add_subscription(&sub, gateway_node);
        debug!(
            conn_id = %sub.conn_id, sub_id = %sub.sub_id,
            topic = %sub.topic, "Subscription registered"
        );
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
    use realtime_core::{
        ConnectionId, EventEnvelope, FilterExpr, SubConfig, SubscriptionId, TopicPath, TopicPattern,
    };
    use smol_str::SmolStr;

    fn make_sub(
        conn_id: u64,
        sub_id: &str,
        topic: &str,
        filter: Option<FilterExpr>,
    ) -> Subscription {
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

        let event = EventEnvelope::new(TopicPath::new("broadcast"), "notify", Bytes::from("{}"));
        let matches = registry.lookup_matches(&event);
        assert_eq!(matches.len(), 100);
    }
}

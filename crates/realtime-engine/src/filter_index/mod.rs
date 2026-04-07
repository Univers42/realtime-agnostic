//! Bitmap-based inverted index for high-cardinality filter evaluation.
//!
//! When thousands of subscriptions exist, evaluating each filter individually
//! is O(S) per event. The [`FilterIndex`] pre-indexes filter predicates into
//! [`RoaringBitmap`]s so that evaluation becomes a series of bitmap operations
//! — dramatically faster for large subscription sets.
//!
//! ## Slot-based dispatch
//!
//! Instead of storing connection IDs in the bitmap and then doing O(n) `DashMap`
//! lookups to resolve dispatch info, we store **slot IDs** that index directly
//! into a `Vec<Option<DispatchSlot>>` slab (~2ns per slot vs ~30ns `DashMap`).
//!
//! The index uses flat composite keys `"pattern\0field\0value"` to avoid
//! nested `DashMap` lock contention.

mod evaluate;
mod mutate;

use std::borrow::Cow;
use std::sync::{Mutex, RwLock};

use dashmap::{DashMap, DashSet};
use realtime_core::{
    filter::{FilterExpr, FilterValue},
    ConnectionId, NodeId, SubscriptionId, TopicPattern,
};
use roaring::RoaringBitmap;

/// Pre-computed dispatch information for a single subscription.
///
/// Stored in the slab at the slot ID tracked by the bitmap, enabling
/// O(1) lookup during event dispatch without any `DashMap` access.
#[derive(Debug, Clone)]
pub struct DispatchSlot {
    /// Connection to dispatch to.
    pub conn_id: ConnectionId,
    /// Subscription identifier.
    pub sub_id: SubscriptionId,
    /// Gateway node for multi-node routing.
    pub gateway_node: Option<NodeId>,
    /// Topic pattern for this subscription.
    pub topic: TopicPattern,
    /// Optional filter expression (needed for post-filtering non-exact matches).
    pub filter: Option<FilterExpr>,
    /// Whether the bitmap result is exact (no post-filter needed).
    ///
    /// `true` for `None` (unfiltered), `Eq`, `In`, and pure `Or` trees.
    /// `false` for `And`, `Ne`, `Not` (bitmap over-approximates).
    pub bitmap_exact: bool,
}

/// Bitmap-based inverted index for efficient filter evaluation at scale.
///
/// Uses flat composite keys (`"pattern\0field\0value"`) with slot-based
/// dispatch for sub-linear event routing. The bitmap stores slot IDs that
/// index directly into a `Vec<Option<DispatchSlot>>` slab, eliminating
/// per-connection `DashMap` lookups from the hot path.
pub struct FilterIndex {
    /// Flat inverted index: `"pattern\0field\0value"` → bitmap of slot IDs.
    index: DashMap<String, RoaringBitmap>,
    /// Unfiltered subscriptions (no filter / non-indexable filters).
    unfiltered: DashMap<String, RoaringBitmap>,
    /// All registered patterns for matching incoming events.
    patterns: DashMap<String, TopicPattern>,
    /// Fields indexed per pattern (for targeted evaluation lookups).
    fields_by_pattern: DashMap<String, DashSet<String>>,
    /// Per-subscription tracked index keys for O(k) targeted removal.
    sub_keys: DashMap<(ConnectionId, SubscriptionId), Vec<String>>,
}

impl FilterIndex {
    /// Create a new empty filter index.
    #[must_use]
    pub fn new() -> Self {
        Self {
            index: DashMap::new(),
            unfiltered: DashMap::new(),
            patterns: DashMap::new(),
        }
    }
}

impl Default for FilterIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use realtime_core::{
        filter::{FieldPath, FilterExpr, FilterValue},
        ConnectionId, EventEnvelope, SubConfig, Subscription, SubscriptionId, TopicPath,
        TopicPattern,
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
    fn test_unfiltered_bitmap() {
        let index = FilterIndex::new();
        for i in 0..100 {
            let sub = make_sub(i, &format!("sub-{i}"), "broadcast", None);
            index.add_subscription(&sub);
        }

        let event = EventEnvelope::new(TopicPath::new("broadcast"), "notify", Bytes::from("{}"));
        let bitmap = index.evaluate(&event);
        assert_eq!(bitmap.len(), 100);
    }

    #[test]
    fn test_filtered_bitmap() {
        let index = FilterIndex::new();
        for i in 0..50 {
            let filter = FilterExpr::Eq(
                FieldPath::new("event_type"),
                FilterValue::String("created".to_string()),
            );
            let sub = make_sub(i, &format!("sub-{i}"), "orders/*", Some(filter));
            index.add_subscription(&sub);
        }
        for i in 50..100 {
            let filter = FilterExpr::Eq(
                FieldPath::new("event_type"),
                FilterValue::String("deleted".to_string()),
            );
            let sub = make_sub(i, &format!("sub-{i}"), "orders/*", Some(filter));
            index.add_subscription(&sub);
        }

        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let bitmap = index.evaluate(&event);
        assert_eq!(bitmap.len(), 50);
        for id in &bitmap {
            assert!(id < 50, "Only conn_ids 0-49 should match 'created'");
        }
    }

    #[test]
    fn test_in_filter_bitmap() {
        let index = FilterIndex::new();
        let filter = FilterExpr::In(
            FieldPath::new("event_type"),
            vec![
                FilterValue::String("created".to_string()),
                FilterValue::String("updated".to_string()),
            ],
        );
        let sub = make_sub(1, "sub-1", "orders/*", Some(filter));
        index.add_subscription(&sub);

        let event1 = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let event2 = EventEnvelope::new(
            TopicPath::new("orders/updated"),
            "updated",
            Bytes::from("{}"),
        );
        let event3 = EventEnvelope::new(
            TopicPath::new("orders/deleted"),
            "deleted",
            Bytes::from("{}"),
        );
        assert_eq!(index.evaluate(&event1).len(), 1);
        assert_eq!(index.evaluate(&event2).len(), 1);
        assert_eq!(index.evaluate(&event3).len(), 0);
    }

    #[test]
    fn test_remove_subscription() {
        let index = FilterIndex::new();
        let sub = make_sub(1, "sub-1", "orders/created", None);
        index.add_subscription(&sub);

        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        assert_eq!(index.evaluate(&event).len(), 1);
        index.remove_subscription(&sub);
        assert_eq!(index.evaluate(&event).len(), 0);
    }
}

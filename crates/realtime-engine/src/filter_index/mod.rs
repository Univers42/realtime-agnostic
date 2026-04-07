//! Bitmap-based inverted index for high-cardinality filter evaluation.
//!
//! When thousands of subscriptions exist, evaluating each filter individually
//! is O(S) per event. The [`FilterIndex`] pre-indexes filter predicates into
//! [`RoaringBitmap`]s so that evaluation becomes a series of bitmap
//! intersections — dramatically faster for large subscription sets.
//!
//! ## How it works
//!
//! ```text
//! topic_pattern → field_name → field_value → RoaringBitmap of conn_ids
//! ```

mod evaluate;
mod mutate;

use dashmap::DashMap;
use realtime_core::TopicPattern;
use roaring::RoaringBitmap;

/// Bitmap-based inverted index for efficient filter evaluation at scale.
///
/// Maintains a three-level nested map:
/// ```text
/// topic_pattern → field_name → field_value → RoaringBitmap of conn_ids
/// ```
///
/// Plus an `unfiltered` map for subscriptions with no filter (they match
/// all events on their topic pattern).
pub struct FilterIndex {
    /// Filtered subscriptions: topic → field → value → bitmap.
    index: DashMap<String, DashMap<String, DashMap<String, RoaringBitmap>>>,
    /// Unfiltered subscriptions (no filter = wildcard match).
    unfiltered: DashMap<String, RoaringBitmap>,
    /// All registered patterns for matching incoming events.
    patterns: DashMap<String, TopicPattern>,
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

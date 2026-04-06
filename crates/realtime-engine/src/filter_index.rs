/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   filter_index.rs                                    :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:00 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:14:21 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

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
//!
//! At **subscribe** time: filter predicates are decomposed and each
//! `(field, value)` pair gets the connection ID inserted into the
//! corresponding bitmap.
//!
//! At **event** time: the event's field values are looked up in the
//! index and the matching bitmaps are OR-ed together.

use dashmap::DashMap;
use realtime_core::{
    filter::{envelope_field_getter, FieldPath, FilterExpr, FilterValue}, EventEnvelope, Subscription, TopicPattern,
};
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
    /// topic_pattern_str → bitmap of connection IDs.
    unfiltered: DashMap<String, RoaringBitmap>,

    /// All registered patterns for matching incoming events.
    patterns: DashMap<String, TopicPattern>,
}

impl FilterIndex {
    /// Create a new empty filter index.
    pub fn new() -> Self {
        Self {
            index: DashMap::new(),
            unfiltered: DashMap::new(),
            patterns: DashMap::new(),
        }
    }

    /// Index a subscription's filter predicates into the bitmap structure.
    ///
    /// If the subscription has no filter, it goes into the `unfiltered` map.
    /// `NOT` and `NE` predicates cannot be efficiently bitmap-indexed, so
    /// those connections are placed in the `unfiltered` set as well.
    pub fn add_subscription(&self, sub: &Subscription) {
        let pattern_key = sub.topic.as_str().to_string();
        let conn_id = sub.conn_id.0 as u32;

        // Store the pattern
        self.patterns
            .entry(pattern_key.clone())
            .or_insert_with(|| sub.topic.clone());

        if let Some(ref filter) = sub.filter {
            self.index_filter(&pattern_key, conn_id, filter);
        } else {
            // No filter — this connection matches all events on this topic
            self.unfiltered
                .entry(pattern_key)
                .or_default()
                .insert(conn_id);
        }
    }

    /// Remove a subscription from all bitmap indexes.
    pub fn remove_subscription(&self, sub: &Subscription) {
        let pattern_key = sub.topic.as_str().to_string();
        let conn_id = sub.conn_id.0 as u32;

        // Remove from unfiltered
        if let Some(mut bitmap) = self.unfiltered.get_mut(&pattern_key) {
            bitmap.remove(conn_id);
        }

        // Remove from filtered index
        if let Some(field_index) = self.index.get(&pattern_key) {
            for field_entry in field_index.iter() {
                for mut value_entry in field_entry.value().iter_mut() {
                    value_entry.value_mut().remove(conn_id);
                }
            }
        }
    }

    /// Evaluate all filters against an event, returning a [`RoaringBitmap`]
    /// of connection IDs that should receive this event.
    ///
    /// Steps:
    /// 1. For each registered pattern that matches the event's topic:
    ///    a. OR in all unfiltered connection bitmaps.
    ///    b. Look up each field value in the inverted index and OR in matches.
    /// 2. Return the union bitmap.
    pub fn evaluate(&self, event: &EventEnvelope) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();

        for pattern_ref in self.patterns.iter() {
            let pattern = pattern_ref.value();
            if !pattern.matches(&event.topic) {
                continue;
            }

            let pattern_key = pattern_ref.key();

            // Add all unfiltered subscriptions for this pattern
            if let Some(unfiltered) = self.unfiltered.get(pattern_key.as_str()) {
                result |= unfiltered.value();
            }

            // Evaluate filtered subscriptions using bitmap intersection
            if let Some(field_index) = self.index.get(pattern_key.as_str()) {
                let filtered_result = self.evaluate_field_index(&field_index, event);
                result |= &filtered_result;
            }
        }

        result
    }

    /// Recursively walk a [`FilterExpr`] tree and insert bitmap entries
    /// for each indexable predicate (Eq, In).
    fn index_filter(&self, pattern_key: &str, conn_id: u32, filter: &FilterExpr) {
        match filter {
            FilterExpr::Eq(field, value) => {
                if let Some(value_str) = self.value_to_string(value) {
                    self.insert_index(pattern_key, &field.0, &value_str, conn_id);
                }
            }
            FilterExpr::In(field, values) => {
                for value in values {
                    if let Some(value_str) = self.value_to_string(value) {
                        self.insert_index(pattern_key, &field.0, &value_str, conn_id);
                    }
                }
            }
            FilterExpr::And(left, right) => {
                self.index_filter(pattern_key, conn_id, left);
                self.index_filter(pattern_key, conn_id, right);
            }
            FilterExpr::Or(left, right) => {
                self.index_filter(pattern_key, conn_id, left);
                self.index_filter(pattern_key, conn_id, right);
            }
            FilterExpr::Not(_inner) => {
                // NOT predicates can't be efficiently indexed; add to unfiltered
                // and let the final filter evaluation handle it.
                self.unfiltered
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(conn_id);
            }
            FilterExpr::Ne(_, _) => {
                // NE predicates: same as NOT — add to unfiltered
                self.unfiltered
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(conn_id);
            }
        }
    }

    /// Insert a single (pattern, field, value, conn_id) entry into the index.
    fn insert_index(&self, pattern_key: &str, field: &str, value: &str, conn_id: u32) {
        self.index
            .entry(pattern_key.to_string())
            .or_default()
            .entry(field.to_string())
            .or_default()
            .entry(value.to_string())
            .or_default()
            .insert(conn_id);
    }

    /// Convert a [`FilterValue`] to a string for use as a hashmap key.
    fn value_to_string(&self, value: &FilterValue) -> Option<String> {
        match value {
            FilterValue::String(s) => Some(s.clone()),
            FilterValue::Integer(i) => Some(i.to_string()),
            FilterValue::Float(f) => Some(f.to_string()),
            FilterValue::Bool(b) => Some(b.to_string()),
            FilterValue::Null => Some("null".to_string()),
        }
    }

    /// For a given field-level sub-index and event, produce a bitmap
    /// of connections whose filter predicates match the event's field values.
    fn evaluate_field_index(
        &self,
        field_index: &DashMap<String, DashMap<String, RoaringBitmap>>,
        event: &EventEnvelope,
    ) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();

        for field_entry in field_index.iter() {
            let field_name = field_entry.key();
            let field_path = FieldPath::new(field_name.clone());

            if let Some(field_value) = envelope_field_getter(event, &field_path) {
                if let Some(value_str) = self.value_to_string(&field_value) {
                    if let Some(bitmap) = field_entry.value().get(&value_str) {
                        result |= bitmap.value();
                    }
                }
            }
        }

        result
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
    use realtime_core::{ConnectionId, SubConfig, SubscriptionId, TopicPath};
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
    fn test_unfiltered_bitmap() {
        let index = FilterIndex::new();

        for i in 0..100 {
            let sub = make_sub(i, &format!("sub-{i}"), "broadcast", None);
            index.add_subscription(&sub);
        }

        let event = EventEnvelope::new(
            TopicPath::new("broadcast"),
            "notify",
            Bytes::from("{}"),
        );

        let bitmap = index.evaluate(&event);
        assert_eq!(bitmap.len(), 100);
    }

    #[test]
    fn test_filtered_bitmap() {
        let index = FilterIndex::new();

        // 50 subscribers want "created" events
        for i in 0..50 {
            let filter = FilterExpr::Eq(
                FieldPath::new("event_type"),
                FilterValue::String("created".to_string()),
            );
            let sub = make_sub(i, &format!("sub-{i}"), "orders/*", Some(filter));
            index.add_subscription(&sub);
        }

        // 50 subscribers want "deleted" events
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

        // Verify the correct connections matched
        for id in bitmap.iter() {
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

//! Lookup operations — the hot path queried on every incoming event.

use realtime_core::{ConnectionId, EventEnvelope, SubscriptionId};

use super::{SubscriptionEntry, SubscriptionRegistry};

impl SubscriptionRegistry {
    /// Look up all matching connections for an event, applying server-side filters.
    ///
    /// This is the **hot path** of the engine. For each registered topic
    /// pattern that matches, every subscription is checked against its filter.
    pub fn lookup_matches(
        &self,
        event: &EventEnvelope,
    ) -> Vec<(ConnectionId, SubscriptionId, Option<realtime_core::NodeId>)> {
        let mut matches = Vec::new();
        for pref in &self.patterns {
            if !pref.value().matches(&event.topic) {
                continue;
            }
            let Some(topic_subs) = self.by_topic.get(pref.key().as_str()) else {
                continue;
            };
            for (conn_id, sub_id) in topic_subs.iter() {
                let key = (*conn_id, sub_id.0.to_string());
                let Some(entry) = self.by_sub_id.get(&key) else {
                    continue;
                };
                if let Some(ref f) = entry.subscription.filter {
                    let getter = |fld: &realtime_core::filter::FieldPath| {
                        realtime_core::filter::envelope_field_getter(event, fld)
                    };
                    if !f.evaluate(&getter) {
                        continue;
                    }
                }
                matches.push((*conn_id, sub_id.clone(), entry.gateway_node));
            }
        }
        matches
    }

    /// Optimized lookup using the bitmap filter index.
    ///
    /// For high-cardinality scenarios (10k+ subscriptions), the bitmap
    /// approach can be faster than iterating all subscriptions.
    pub fn lookup_matches_bitmap(
        &self,
        event: &EventEnvelope,
    ) -> Vec<(ConnectionId, SubscriptionId, Option<realtime_core::NodeId>)> {
        let bitmap = self.filter_index.evaluate(event);
        let mut matches = Vec::new();
        for raw in &bitmap {
            let conn_id = ConnectionId(u64::from(raw));
            let Some(subs) = self.by_connection.get(&conn_id) else {
                continue;
            };
            for entry in subs.iter() {
                if entry.subscription.topic.matches(&event.topic) {
                    let sub_id = entry.subscription.sub_id.clone();
                    matches.push((conn_id, sub_id, entry.gateway_node));
                }
            }
        }
        matches
    }

    /// Return a clone of all subscriptions for a given connection.
    #[must_use]
    pub fn get_connection_subscriptions(&self, conn_id: ConnectionId) -> Vec<SubscriptionEntry> {
        self.by_connection
            .get(&conn_id)
            .map(|v| v.clone())
            .unwrap_or_default()
    }

    /// Return the current sequence for a topic (placeholder — managed by `SequenceGenerator`).
    #[must_use]
    pub const fn get_topic_sequence(&self, _topic: &str) -> u64 {
        0
    }
}

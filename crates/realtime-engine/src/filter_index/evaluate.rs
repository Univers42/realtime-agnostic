//! Evaluation and dispatch operations — the hot path of the engine.
//!
//! [`evaluate()`] returns a [`RoaringBitmap`] of slot IDs. [`for_each_match()`]
//! combines evaluation with dispatch, invoking a callback for each matching
//! slot using the pre-computed [`DispatchSlot`] info. JSON payload is parsed
//! once and shared between bitmap evaluation and any post-filter checks.

use realtime_core::{
    filter::{envelope_field_getter_cached, FieldPath},
    ConnectionId, EventEnvelope, NodeId, SubscriptionId,
};
use roaring::RoaringBitmap;

use super::FilterIndex;

impl FilterIndex {
    /// Evaluate all filters against an event, returning a bitmap of slot IDs.
    pub fn evaluate(&self, event: &EventEnvelope) -> RoaringBitmap {
        // Lazy parse: skip JSON deserialization when no fields are indexed.
        let parsed: Option<serde_json::Value> = if self.fields_by_pattern.is_empty() {
            None
        } else {
            serde_json::from_slice(&event.payload).ok()
        };
        self.evaluate_inner(event, parsed.as_ref())
    }

    /// Internal evaluation using a pre-parsed payload (avoids double-parse
    /// when called from [`for_each_match()`]).
    fn evaluate_inner(
        &self,
        event: &EventEnvelope,
        parsed_payload: Option<&serde_json::Value>,
    ) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        let mut key_buf = String::with_capacity(128);

        for pattern_ref in &self.patterns {
            if !pattern_ref.value().matches(&event.topic) {
                continue;
            }
            let pattern_key = pattern_ref.key();

            // Include all unfiltered (and Ne/Not) subscriptions.
            if let Some(unfiltered) = self.unfiltered.get(pattern_key.as_str()) {
                result |= unfiltered.value();
            }

            // Look up indexed fields using flat composite keys.
            if let Some(fields) = self.fields_by_pattern.get(pattern_key.as_str()) {
                for field_path in fields.value() {
                    if let Some(fv) = envelope_field_getter_cached(
                        event,
                        field_path,
                        parsed_payload,
                    ) {
                        let vs = Self::value_to_string(&fv);
                        key_buf.clear();
                        key_buf.push_str(pattern_key);
                        key_buf.push('\0');
                        key_buf.push_str(&field_path.0);
                        key_buf.push('\0');
                        key_buf.push_str(&vs);
                        if let Some(bitmap) = self.index.get(key_buf.as_str()) {
                            result |= bitmap.value();
                        }
                    }
                }
            }
        }
        result
    }

    /// Iterate all matching subscriptions for an event, invoking `callback`
    /// for each match. Returns the number of matches.
    ///
    /// For `bitmap_exact` slots, the callback is invoked immediately.
    /// For non-exact slots, the filter is re-evaluated using the pre-parsed
    /// payload. JSON is parsed exactly **once** regardless of slot count.
    pub fn for_each_match<F>(&self, event: &EventEnvelope, mut callback: F) -> usize
    where
        F: FnMut(ConnectionId, &SubscriptionId, Option<NodeId>),
    {
        // Lazy JSON parse: skip when there are no indexed fields.
        // Non-exact slots that need post-filtering will trigger a late parse.
        let has_fields = !self.fields_by_pattern.is_empty();
        let parsed: Option<serde_json::Value> = if has_fields {
            serde_json::from_slice(&event.payload).ok()
        } else {
            None
        };
        let bitmap = self.evaluate_inner(event, parsed.as_ref());
        if bitmap.is_empty() {
            return 0;
        }

        // Late parse is deferred until the first non-exact slot needs it.
        let mut late_parsed: Option<serde_json::Value> = None;

        let slots = self.slots.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut count = 0;

        for slot_id in &bitmap {
            // SAFETY: slot_id was inserted by alloc_slot and is always < slots.len().
            let entry = unsafe { slots.get_unchecked(slot_id as usize) };
            if let Some(slot) = entry {
                if slot.bitmap_exact {
                    // Fast path: bitmap is exact, no post-filter needed.
                    callback(slot.conn_id, &slot.sub_id, slot.gateway_node);
                    count += 1;
                } else if let Some(ref f) = slot.filter {
                    // Parse payload on demand for non-exact post-filtering.
                    if late_parsed.is_none() && parsed.is_none() {
                        late_parsed = serde_json::from_slice(&event.payload).ok();
                    }
                    let parsed_ref = parsed.as_ref().or(late_parsed.as_ref());
                    let getter = |fld: &FieldPath| {
                        envelope_field_getter_cached(event, fld, parsed_ref)
                    };
                    if f.evaluate(&getter) {
                        callback(slot.conn_id, &slot.sub_id, slot.gateway_node);
                        count += 1;
                    }
                } else {
                    callback(slot.conn_id, &slot.sub_id, slot.gateway_node);
                    count += 1;
                }
            }
        }
        count
    }

    /// Collect all matching subscriptions into a pre-allocated `Vec`.
    ///
    /// Like [`for_each_match()`] but returns a `Vec` sized to the bitmap
    /// length upfront, avoiding ~14 reallocations at 10K matches.
    pub fn collect_matches(
        &self,
        event: &EventEnvelope,
    ) -> Vec<(ConnectionId, SubscriptionId, Option<NodeId>)> {
        let has_fields = !self.fields_by_pattern.is_empty();
        let parsed: Option<serde_json::Value> = if has_fields {
            serde_json::from_slice(&event.payload).ok()
        } else {
            None
        };
        let bitmap = self.evaluate_inner(event, parsed.as_ref());
        if bitmap.is_empty() {
            return Vec::new();
        }

        let mut late_parsed: Option<serde_json::Value> = None;

        #[allow(clippy::cast_possible_truncation)]
        let mut matches = Vec::with_capacity(bitmap.len() as usize);
        let slots = self.slots.read().unwrap_or_else(std::sync::PoisonError::into_inner);

        for slot_id in &bitmap {
            // SAFETY: slot_id was inserted by alloc_slot and is always < slots.len().
            let entry = unsafe { slots.get_unchecked(slot_id as usize) };
            if let Some(slot) = entry {
                if slot.bitmap_exact {
                    matches.push((slot.conn_id, slot.sub_id.clone(), slot.gateway_node));
                } else if let Some(ref f) = slot.filter {
                    if late_parsed.is_none() && parsed.is_none() {
                        late_parsed = serde_json::from_slice(&event.payload).ok();
                    }
                    let parsed_ref = parsed.as_ref().or(late_parsed.as_ref());
                    let getter = |fld: &FieldPath| {
                        envelope_field_getter_cached(event, fld, parsed_ref)
                    };
                    if f.evaluate(&getter) {
                        matches.push((slot.conn_id, slot.sub_id.clone(), slot.gateway_node));
                    }
                } else {
                    matches.push((slot.conn_id, slot.sub_id.clone(), slot.gateway_node));
                }
            }
        }
        matches
    }
}

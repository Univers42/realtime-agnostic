//! Evaluation operations — match events against the bitmap index.

use dashmap::DashMap;
use realtime_core::{
    filter::{envelope_field_getter, FieldPath, FilterValue},
    EventEnvelope,
};
use roaring::RoaringBitmap;

use super::FilterIndex;

impl FilterIndex {
    /// Evaluate all filters against an event, returning a [`RoaringBitmap`]
    /// of connection IDs that should receive this event.
    pub fn evaluate(&self, event: &EventEnvelope) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for pattern_ref in &self.patterns {
            if !pattern_ref.value().matches(&event.topic) {
                continue;
            }
            let pattern_key = pattern_ref.key();
            if let Some(unfiltered) = self.unfiltered.get(pattern_key.as_str()) {
                result |= unfiltered.value();
            }
            if let Some(field_index) = self.index.get(pattern_key.as_str()) {
                result |= &self.evaluate_field_index(&field_index, event);
            }
        }
        result
    }

    /// Convert a [`FilterValue`] to a string for use as a hashmap key.
    pub(crate) fn value_to_string(value: &FilterValue) -> String {
        match value {
            FilterValue::String(s) => s.clone(),
            FilterValue::Integer(i) => i.to_string(),
            FilterValue::Float(f) => f.to_string(),
            FilterValue::Bool(b) => b.to_string(),
            FilterValue::Null => "null".to_string(),
        }
    }

    /// For a given field-level sub-index and event, produce a bitmap
    /// of connections whose filter predicates match the event's field values.
    #[allow(clippy::unused_self)]
    fn evaluate_field_index(
        &self,
        field_index: &DashMap<String, DashMap<String, RoaringBitmap>>,
        event: &EventEnvelope,
    ) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        for field_entry in field_index {
            let field_path = FieldPath::new(field_entry.key().clone());
            if let Some(fv) = envelope_field_getter(event, &field_path) {
                let vs = Self::value_to_string(&fv);
                if let Some(bitmap) = field_entry.value().get(&vs) {
                    result |= bitmap.value();
                }
            }
        }
        result
    }
}

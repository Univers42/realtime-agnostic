//! Mutation operations — add and remove subscriptions from the bitmap index.
//!
//! Uses slot-based allocation with tracked composite keys for O(k) removal
//! where k = number of filter predicates.

use realtime_core::{filter::FilterExpr, NodeId, Subscription};

use super::FilterIndex;

impl FilterIndex {
    /// Index a subscription into the bitmap structure.
    ///
    /// Allocates a slot in the dispatch slab, indexes filter predicates
    /// using flat composite keys, and tracks all keys for O(k) removal.
    pub fn add_subscription(&self, sub: &Subscription, gateway_node: Option<NodeId>) {
        let pattern_key = sub.topic.as_str().to_string();
        let bitmap_exact = sub
            .filter
            .as_ref()
            .is_none_or(Self::is_filter_exact);

        // Allocate a dispatch slot.
        let slot = super::DispatchSlot {
            conn_id: sub.conn_id,
            sub_id: sub.sub_id.clone(),
            gateway_node,
            topic: sub.topic.clone(),
            filter: sub.filter.clone(),
            bitmap_exact,
        };
        let slot_id = self.alloc_slot(slot);

        // Register the pattern.
        self.patterns
            .entry(pattern_key.clone())
            .or_insert_with(|| sub.topic.clone());

        // Reverse lookup for removal.
        self.slot_by_sub
            .insert((sub.conn_id, sub.sub_id.clone()), slot_id);

        // Index filter predicates.
        if let Some(ref filter) = sub.filter {
            let mut keys = Vec::new();
            self.index_filter(&pattern_key, slot_id, filter, &mut keys);
            self.sub_keys
                .insert((sub.conn_id, sub.sub_id.clone()), keys);
        } else {
            // Unfiltered: always matches on this pattern.
            self.unfiltered
                .entry(pattern_key)
                .or_default()
                .insert(slot_id);
            self.sub_keys
                .insert((sub.conn_id, sub.sub_id.clone()), Vec::new());
        }
    }

    /// Remove a subscription from all bitmap indexes in O(k) time.
    pub fn remove_subscription(&self, sub: &Subscription) {
        let key = (sub.conn_id, sub.sub_id.clone());

        // Look up and free the slot.
        if let Some((_, slot_id)) = self.slot_by_sub.remove(&key) {
            self.free_slot(slot_id);

            // Remove from unfiltered bitmap.
            let pattern_key = sub.topic.as_str().to_string();
            if let Some(mut bitmap) = self.unfiltered.get_mut(&pattern_key) {
                bitmap.remove(slot_id);
            }

            // Remove from tracked index keys — O(k).
            if let Some((_, keys)) = self.sub_keys.remove(&key) {
                for k in &keys {
                    if let Some(mut bitmap) = self.index.get_mut(k) {
                        bitmap.remove(slot_id);
                    }
                }
            }
        }
    }

    /// Allocate a slot in the dispatch slab, reusing free slots.
    fn alloc_slot(&self, slot: super::DispatchSlot) -> u32 {
        let mut free = self.free_slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(id) = free.pop() {
            drop(free);
            let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
            slots[id as usize] = Some(slot);
            id
        } else {
            drop(free);
            let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
            let id = u32::try_from(slots.len()).unwrap_or(u32::MAX);
            slots.push(Some(slot));
            id
        }
    }

    /// Free a slot in the dispatch slab for reuse.
    fn free_slot(&self, slot_id: u32) {
        let mut slots = self.slots.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = slots.get_mut(slot_id as usize) {
            *entry = None;
        }
        drop(slots);
        let mut free = self.free_slots.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        free.push(slot_id);
    }

    /// Recursively walk a [`FilterExpr`] tree and insert bitmap entries.
    fn index_filter(
        &self,
        pattern_key: &str,
        slot_id: u32,
        filter: &FilterExpr,
        tracked_keys: &mut Vec<String>,
    ) {
        match filter {
            FilterExpr::Eq(field, value) => {
                let vs = Self::value_to_string(value);
                let key = Self::make_index_key(pattern_key, &field.0, &vs);
                self.index.entry(key.clone()).or_default().insert(slot_id);
                self.fields_by_pattern
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(field.0.clone());
                tracked_keys.push(key);
            }
            FilterExpr::In(field, values) => {
                for v in values {
                    let vs = Self::value_to_string(v);
                    let key = Self::make_index_key(pattern_key, &field.0, &vs);
                    self.index.entry(key.clone()).or_default().insert(slot_id);
                    tracked_keys.push(key);
                }
                self.fields_by_pattern
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(field.0.clone());
            }
            FilterExpr::And(l, r) | FilterExpr::Or(l, r) => {
                self.index_filter(pattern_key, slot_id, l, tracked_keys);
                self.index_filter(pattern_key, slot_id, r, tracked_keys);
            }
            FilterExpr::Not(_) | FilterExpr::Ne(_, _) => {
                // Non-indexable: goes to unfiltered, will be post-filtered.
                self.unfiltered
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(slot_id);
            }
        }
    }
}

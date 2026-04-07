//! Mutation operations — add and remove subscriptions from the bitmap index.

use realtime_core::{filter::FilterExpr, Subscription};

use super::FilterIndex;

impl FilterIndex {
    /// Index a subscription's filter predicates into the bitmap structure.
    ///
    /// If the subscription has no filter, it goes into the `unfiltered` map.
    #[allow(clippy::cast_possible_truncation)]
    pub fn add_subscription(&self, sub: &Subscription) {
        let pattern_key = sub.topic.as_str().to_string();
        let conn_id = sub.conn_id.0 as u32;
        self.patterns
            .entry(pattern_key.clone())
            .or_insert_with(|| sub.topic.clone());
        if let Some(ref filter) = sub.filter {
            self.index_filter(&pattern_key, conn_id, filter);
        } else {
            self.unfiltered
                .entry(pattern_key)
                .or_default()
                .insert(conn_id);
        }
    }

    /// Remove a subscription from all bitmap indexes.
    #[allow(clippy::cast_possible_truncation)]
    pub fn remove_subscription(&self, sub: &Subscription) {
        let pattern_key = sub.topic.as_str().to_string();
        let conn_id = sub.conn_id.0 as u32;
        if let Some(mut bitmap) = self.unfiltered.get_mut(&pattern_key) {
            bitmap.remove(conn_id);
        }
        if let Some(field_index) = self.index.get(&pattern_key) {
            for field_entry in field_index.iter() {
                for mut value_entry in field_entry.value().iter_mut() {
                    value_entry.value_mut().remove(conn_id);
                }
            }
        }
    }

    /// Recursively walk a [`FilterExpr`] tree and insert bitmap entries
    /// for each indexable predicate (Eq, In).
    fn index_filter(&self, pattern_key: &str, conn_id: u32, filter: &FilterExpr) {
        match filter {
            FilterExpr::Eq(field, value) => {
                let vs = Self::value_to_string(value);
                self.insert_index(pattern_key, &field.0, &vs, conn_id);
            }
            FilterExpr::In(field, values) => {
                for v in values {
                    let vs = Self::value_to_string(v);
                    self.insert_index(pattern_key, &field.0, &vs, conn_id);
                }
            }
            FilterExpr::And(l, r) | FilterExpr::Or(l, r) => {
                self.index_filter(pattern_key, conn_id, l);
                self.index_filter(pattern_key, conn_id, r);
            }
            FilterExpr::Not(_) | FilterExpr::Ne(_, _) => {
                self.unfiltered
                    .entry(pattern_key.to_string())
                    .or_default()
                    .insert(conn_id);
            }
        }
    }

    /// Insert a single (pattern, field, value, `conn_id`) entry into the index.
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
}

/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   filter.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:11:35 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:14:21 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Server-side event filter expressions.
//!
//! Clients can attach a filter to their subscriptions so that only
//! events matching the filter are delivered. Filters are evaluated on
//! the server (inside the [`FilterIndex`](crate::filter::FilterExpr)),
//! saving bandwidth and client-side CPU.
//!
//! ## Supported operators
//!
//! | Operator | JSON syntax              | Description              |
//! |----------|--------------------------|--------------------------|
//! | `eq`     | `{ "field": { "eq": V }}` | Field equals value       |
//! | `ne`     | `{ "field": { "ne": V }}` | Field not equals value   |
//! | `in`     | `{ "field": { "in": [..] }}` | Field is one of values |
//!
//! Multiple conditions are implicitly ANDed.
//!
//! ## Field paths
//!
//! Filters can reference envelope-level fields (`event_type`, `topic`,
//! `source.kind`, `source.id`) **and** payload fields (`payload.user.name`
//! or just `user.name` for convenience). Payload fields are extracted by
//! parsing the JSON payload at evaluation time.

use serde::{Deserialize, Serialize};

/// A composable, tree-structured filter expression.
///
/// Built from JSON at subscribe time (via [`from_json()`](Self::from_json))
/// and evaluated against each event using [`evaluate()`](Self::evaluate).
/// Uses a boolean algebra of `And`, `Or`, `Not` nodes with `Eq`, `Ne`,
/// `In` leaf nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FilterExpr {
    /// Field equals value: `{ "event_type": { "eq": "created" } }`
    Eq(FieldPath, FilterValue),
    /// Field not equals value.
    Ne(FieldPath, FilterValue),
    /// Field is one of several values: `{ "event_type": { "in": ["created","updated"] } }`
    In(FieldPath, Vec<FilterValue>),
    /// Both sub-expressions must be true.
    And(Box<FilterExpr>, Box<FilterExpr>),
    /// At least one sub-expression must be true.
    Or(Box<FilterExpr>, Box<FilterExpr>),
    /// Inverts the inner expression.
    Not(Box<FilterExpr>),
}

/// A dot-separated path identifying a field on the [`EventEnvelope`](crate::types::EventEnvelope).
///
/// ## Supported paths
///
/// | Path                     | Resolves to                                  |
/// |--------------------------|----------------------------------------------|
/// | `event_type`             | `envelope.event_type`                        |
/// | `topic`                  | `envelope.topic` (as string)                 |
/// | `source.kind`            | `envelope.source.kind` (debug string)        |
/// | `source.id`              | `envelope.source.id`                         |
/// | `source.metadata.{key}`  | `envelope.source.metadata[key]`              |
/// | `payload.{path}`         | JSON field inside payload bytes               |
/// | `{bare_name}`            | Shorthand for `payload.{bare_name}`          |
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldPath(pub String);

impl FieldPath {
    /// Create a new field path from a string.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// A typed value used in filter comparisons.
///
/// Supports the JSON primitive types. Uses `#[serde(untagged)]`
/// so JSON values deserialize naturally (strings as `String`,
/// numbers as `Integer` or `Float`, etc.).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FilterValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bool(bool),
    Null,
}

impl FilterValue {
    /// Try to extract the inner string, returning `None` for non-string values.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            FilterValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl FilterExpr {
    /// Evaluate this filter expression against an event.
    ///
    /// The `field_getter` closure extracts field values from the event.
    /// Use [`envelope_field_getter()`] as the standard implementation.
    ///
    /// # Arguments
    ///
    /// * `field_getter` — A closure `(&FieldPath) -> Option<FilterValue>`
    ///   that extracts a field's value from the target event.
    ///
    /// # Returns
    ///
    /// `true` if the event passes the filter.
    pub fn evaluate(&self, field_getter: &dyn Fn(&FieldPath) -> Option<FilterValue>) -> bool {
        match self {
            FilterExpr::Eq(field, expected) => {
                field_getter(field).as_ref() == Some(expected)
            }
            FilterExpr::Ne(field, expected) => {
                field_getter(field).as_ref() != Some(expected)
            }
            FilterExpr::In(field, values) => {
                if let Some(actual) = field_getter(field) {
                    values.contains(&actual)
                } else {
                    false
                }
            }
            FilterExpr::And(left, right) => {
                left.evaluate(field_getter) && right.evaluate(field_getter)
            }
            FilterExpr::Or(left, right) => {
                left.evaluate(field_getter) || right.evaluate(field_getter)
            }
            FilterExpr::Not(inner) => !inner.evaluate(field_getter),
        }
    }

    /// Parse a JSON object into a `FilterExpr` tree.
    ///
    /// ## Supported JSON formats
    ///
    /// ```json
    /// { "event_type": { "eq": "created" } }
    /// { "event_type": { "in": ["created", "updated"] } }
    /// { "event_type": { "ne": "deleted" }, "source.id": { "eq": "pg" } }
    /// ```
    ///
    /// Multiple top-level fields are implicitly ANDed together.
    ///
    /// # Returns
    ///
    /// `Some(FilterExpr)` on success, `None` if the JSON is empty or
    /// contains unknown operators.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let obj = value.as_object()?;
        let mut exprs: Vec<FilterExpr> = Vec::new();

        for (field, condition) in obj {
            let cond_obj = condition.as_object()?;
            for (op, val) in cond_obj {
                let field_path = FieldPath::new(field.clone());
                let expr = match op.as_str() {
                    "eq" => FilterExpr::Eq(field_path, json_to_filter_value(val)),
                    "ne" => FilterExpr::Ne(field_path, json_to_filter_value(val)),
                    "in" => {
                        let arr = val.as_array()?;
                        let values: Vec<FilterValue> =
                            arr.iter().map(json_to_filter_value).collect();
                        FilterExpr::In(field_path, values)
                    }
                    _ => return None,
                };
                exprs.push(expr);
            }
        }

        if exprs.is_empty() {
            None
        } else if exprs.len() == 1 {
            Some(exprs.remove(0))
        } else {
            let mut combined = exprs.remove(0);
            for expr in exprs {
                combined = FilterExpr::And(Box::new(combined), Box::new(expr));
            }
            Some(combined)
        }
    }
}

/// Convert a raw `serde_json::Value` into a [`FilterValue`].
///
/// Used internally by [`FilterExpr::from_json()`].
fn json_to_filter_value(v: &serde_json::Value) -> FilterValue {
    match v {
        serde_json::Value::String(s) => FilterValue::String(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                FilterValue::Integer(i)
            } else if let Some(f) = n.as_f64() {
                FilterValue::Float(f)
            } else {
                FilterValue::Null
            }
        }
        serde_json::Value::Bool(b) => FilterValue::Bool(*b),
        _ => FilterValue::Null,
    }
}

/// Extract a field value from an [`EventEnvelope`](crate::types::EventEnvelope)
/// by [`FieldPath`].
///
/// This is the standard `field_getter` passed to [`FilterExpr::evaluate()`].
/// It supports envelope-level fields (`event_type`, `topic`, `source.*`)
/// and payload-level fields (any path inside the JSON payload).
///
/// # Arguments
///
/// * `event` — The event envelope to extract from.
/// * `field` — The field path to look up.
///
/// # Returns
///
/// `Some(FilterValue)` if the field exists, `None` otherwise.
pub fn envelope_field_getter(
    event: &crate::types::EventEnvelope,
    field: &FieldPath,
) -> Option<FilterValue> {
    match field.0.as_str() {
        "event_type" => Some(FilterValue::String(event.event_type.clone())),
        "topic" => Some(FilterValue::String(event.topic.as_str().to_string())),
        "source.kind" => event.source.as_ref().map(|s| {
            FilterValue::String(format!("{:?}", s.kind))
        }),
        "source.id" => event.source.as_ref().map(|s| {
            FilterValue::String(s.id.clone())
        }),
        field_name if field_name.starts_with("source.metadata.") => {
            let key = &field_name["source.metadata.".len()..];
            event.source.as_ref().and_then(|s| {
                s.metadata.get(key).map(|v| FilterValue::String(v.clone()))
            })
        }
        // Fall through to payload field extraction (JSON path)
        field_name if field_name.starts_with("payload.") => {
            let key = &field_name["payload.".len()..];
            extract_payload_field(&event.payload, key)
        }
        // Bare field names also search inside payload for convenience
        field_name => extract_payload_field(&event.payload, field_name),
    }
}

/// Extract a field from a JSON payload byte slice.
///
/// Supports dot-separated nested paths like `"user.name"` which
/// navigates `{ "user": { "name": "Alice" } }`.
///
/// Returns `None` if the payload is not valid JSON or the path
/// doesn't exist.
fn extract_payload_field(payload: &[u8], path: &str) -> Option<FilterValue> {
    let parsed: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let mut current = &parsed;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    json_value_to_filter_value(current)
}

/// Convert a JSON value into a [`FilterValue`], returning `None` for
/// arrays and objects (which are not directly filterable).
fn json_value_to_filter_value(v: &serde_json::Value) -> Option<FilterValue> {
    match v {
        serde_json::Value::String(s) => Some(FilterValue::String(s.clone())),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(FilterValue::Integer(i))
            } else {
                n.as_f64().map(FilterValue::Float)
            }
        }
        serde_json::Value::Bool(b) => Some(FilterValue::Bool(*b)),
        serde_json::Value::Null => Some(FilterValue::Null),
        _ => None, // arrays/objects not directly filterable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EventEnvelope, TopicPath};
    use bytes::Bytes;

    #[test]
    fn test_filter_eq() {
        let filter = FilterExpr::Eq(
            FieldPath::new("event_type"),
            FilterValue::String("created".to_string()),
        );
        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let result = filter.evaluate(&|field| envelope_field_getter(&event, field));
        assert!(result);
    }

    #[test]
    fn test_filter_ne() {
        let filter = FilterExpr::Ne(
            FieldPath::new("event_type"),
            FilterValue::String("deleted".to_string()),
        );
        let event = EventEnvelope::new(
            TopicPath::new("test"),
            "created",
            Bytes::from("{}"),
        );
        let result = filter.evaluate(&|field| envelope_field_getter(&event, field));
        assert!(result);
    }

    #[test]
    fn test_filter_in() {
        let filter = FilterExpr::In(
            FieldPath::new("event_type"),
            vec![
                FilterValue::String("created".to_string()),
                FilterValue::String("updated".to_string()),
            ],
        );
        let event = EventEnvelope::new(
            TopicPath::new("test"),
            "updated",
            Bytes::from("{}"),
        );
        let result = filter.evaluate(&|field| envelope_field_getter(&event, field));
        assert!(result);
    }

    #[test]
    fn test_filter_and() {
        let filter = FilterExpr::And(
            Box::new(FilterExpr::Eq(
                FieldPath::new("event_type"),
                FilterValue::String("created".to_string()),
            )),
            Box::new(FilterExpr::Eq(
                FieldPath::new("topic"),
                FilterValue::String("orders/created".to_string()),
            )),
        );
        let event = EventEnvelope::new(
            TopicPath::new("orders/created"),
            "created",
            Bytes::from("{}"),
        );
        let result = filter.evaluate(&|field| envelope_field_getter(&event, field));
        assert!(result);
    }

    #[test]
    fn test_filter_from_json() {
        let json = serde_json::json!({
            "event_type": { "in": ["created", "updated"] }
        });
        let filter = FilterExpr::from_json(&json).unwrap();
        let event = EventEnvelope::new(
            TopicPath::new("test"),
            "created",
            Bytes::from("{}"),
        );
        let result = filter.evaluate(&|field| envelope_field_getter(&event, field));
        assert!(result);
    }
}

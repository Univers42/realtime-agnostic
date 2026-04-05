/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   types.rs                                           :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:11:45 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Core type definitions for the Realtime-Agnostic event routing engine.
//!
//! This module defines all the fundamental types used throughout the system:
//! newtypes for type safety, the canonical [`EventEnvelope`], topic patterns,
//! subscription configuration, authentication claims, and dispatch types.

use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use uuid::Uuid;

// ─── Newtypes ────────────────────────────────────────────────────────

/// Globally unique event identifier using UUIDv7 (time-sortable).
///
/// UUIDv7 embeds a millisecond-precision timestamp, making event IDs
/// naturally time-sortable without a separate timestamp field. IDs are
/// globally unique without coordination (no central counter needed).
///
/// # Examples
///
/// ```
/// use realtime_core::EventId;
///
/// let id1 = EventId::new();
/// let id2 = EventId::new();
/// assert_ne!(id1, id2); // Always unique
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId(pub Uuid);

impl EventId {
    /// Generate a new UUIDv7 event identifier.
    ///
    /// Each call produces a globally unique, time-sortable ID.
    /// Thread-safe — can be called from any async task.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique connection identifier assigned by the gateway.
///
/// Uses `u64` internally for memory efficiency (8 bytes vs 16 for UUID).
/// Allocated by `ConnectionManager::next_connection_id()` via atomic increment,
/// guaranteeing uniqueness within a single node.
///
/// Displays as `conn-{N}` for log readability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConnectionId(pub u64);

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conn-{}", self.0)
    }
}

/// Client-assigned subscription identifier, scoped to a connection.
///
/// Uses [`SmolStr`] for stack allocation of short strings (≤23 bytes),
/// avoiding heap allocations for typical subscription IDs like `"s1"` or `"board-events"`.
///
/// Clients choose their own subscription IDs to correlate incoming events
/// with their handlers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SubscriptionId(pub SmolStr);

impl fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Logical topic path used for event routing.
///
/// Topics are hierarchical paths separated by `/`, e.g., `"orders/created"`
/// or `"pg/cards/inserted"`. The first segment is the **namespace** (used for
/// authorization), and subsequent segments describe the event type.
///
/// Uses [`SmolStr`] for stack allocation of short strings (≤23 bytes).
///
/// # Examples
///
/// ```
/// use realtime_core::TopicPath;
///
/// let topic = TopicPath::new("orders/created");
/// assert_eq!(topic.namespace(), "orders");
/// assert_eq!(topic.event_type_part(), "created");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TopicPath(pub SmolStr);

impl TopicPath {
    /// Create a new topic path from a string slice.
    ///
    /// # Arguments
    ///
    /// * `s` — The topic path string, e.g., `"orders/created"` or `"pg/cards/inserted"`.
    pub fn new(s: &str) -> Self {
        Self(SmolStr::new(s))
    }

    /// Return the topic path as a string slice.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the namespace part (everything before the first `/`).
    ///
    /// The namespace is used for authorization: clients can only subscribe
    /// to topics within their allowed namespaces.
    ///
    /// # Examples
    ///
    /// ```
    /// use realtime_core::TopicPath;
    /// assert_eq!(TopicPath::new("orders/created").namespace(), "orders");
    /// assert_eq!(TopicPath::new("pg/cards/inserted").namespace(), "pg");
    /// ```
    pub fn namespace(&self) -> &str {
        self.0.split('/').next().unwrap_or("")
    }

    /// Return the event-type segment (the second path segment, after the first `/`).
    ///
    /// # Examples
    ///
    /// ```
    /// use realtime_core::TopicPath;
    /// assert_eq!(TopicPath::new("orders/created").event_type_part(), "created");
    /// ```
    pub fn event_type_part(&self) -> &str {
        self.0.split('/').nth(1).unwrap_or("")
    }
}

impl fmt::Display for TopicPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Correlation identifier for distributed tracing (e.g., OpenTelemetry trace ID).
///
/// Propagated through the entire event pipeline, allowing end-to-end
/// tracing from producer to consumer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(pub String);

/// Node identifier for gateway/core nodes in a multi-node cluster.
///
/// Used for future horizontal scaling — events can be routed to specific
/// gateway nodes that hold the target connections. Displays as `node-{N}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node-{}", self.0)
    }
}

// ─── Payload Encoding ────────────────────────────────────────────────

/// Content type of an event's payload bytes.
///
/// Serializes as MIME strings (e.g. `"application/json"`) for
/// interoperability with external systems. JSON is the default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayloadEncoding {
    /// JSON payload (`application/json`).
    #[serde(rename = "application/json")]
    Json,
    /// MessagePack payload (`application/msgpack`).
    #[serde(rename = "application/msgpack")]
    MsgPack,
    /// Opaque binary payload (`application/octet-stream`).
    #[serde(rename = "application/octet-stream")]
    Binary,
}

impl Default for PayloadEncoding {
    fn default() -> Self {
        Self::Json
    }
}

// ─── Event Source ────────────────────────────────────────────────────

/// Classification of where an event originated.
///
/// This tag is carried inside [`EventSource`] and helps consumers
/// distinguish CDC events from API-published or scheduled events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    /// Published via the REST or WebSocket API.
    Api,
    /// Generated by a database change (CDC).
    Database,
    /// Generated by a periodic scheduler / cron job.
    Scheduler,
    /// Ingested from an external webhook.
    Webhook,
    /// Application-defined source kind.
    Custom(String),
}

/// Provenance information attached to an event.
///
/// Carries the source kind, a human-readable source ID (e.g. `"pg-producer"`),
/// and optional key-value metadata (e.g. `{ "table": "orders" }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSource {
    /// What kind of system produced this event.
    pub kind: SourceKind,
    /// Human-readable identifier of the producer (e.g. `"pg-main"`).
    pub id: String,
    /// Free-form metadata attached by the producer.
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

// ─── Event Envelope ──────────────────────────────────────────────────

/// Canonical event representation — the *lingua franca* of the engine.
///
/// Every event flowing through the system — whether originating from
/// PostgreSQL CDC, MongoDB change streams, the REST API, or WebSocket
/// clients — is normalized into this envelope before entering the bus.
///
/// ## Design rationale
///
/// * **`Bytes` payload** — reference-counted, zero-copy. Wrapping in `Arc`
///   at the router level means N fan-out copies share the same allocation.
/// * **UUIDv7 `event_id`** — time-sortable, globally unique without coordination.
/// * **`sequence`** — per-topic, monotonically increasing, assigned by the
///   engine's `SequenceGenerator`.
/// * **64 KB payload limit** — enforced at ingestion to prevent memory abuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Globally unique, producer-assigned. UUIDv7 (time-sortable).
    pub event_id: EventId,

    /// Logical topic this event is published to.
    pub topic: TopicPath,

    /// Server-stamped timestamp (RFC 3339).
    pub timestamp: DateTime<Utc>,

    /// Logical sequence number within the topic.
    pub sequence: u64,

    /// Semantic label for the event type.
    pub event_type: String,

    /// Opaque payload bytes. Max 64KB enforced at ingestion.
    #[serde(with = "bytes_serde")]
    pub payload: Bytes,

    /// Content-type of payload bytes.
    pub payload_encoding: PayloadEncoding,

    /// Source that produced this event.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<EventSource>,

    /// Correlation ID for distributed tracing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<TraceId>,

    /// TTL in milliseconds for ephemeral events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u32>,
}

impl EventEnvelope {
    /// Create a new envelope with minimal required fields.
    ///
    /// Automatically generates a UUIDv7 `event_id`, stamps `timestamp` to
    /// `Utc::now()`, sets sequence to 0 (to be assigned by the engine),
    /// and defaults encoding to JSON.
    ///
    /// # Arguments
    ///
    /// * `topic` — The target topic path.
    /// * `event_type` — Semantic label (e.g. `"inserted"`, `"cursor_move"`).
    /// * `payload` — Opaque payload bytes (must be ≤64 KB).
    pub fn new(topic: TopicPath, event_type: impl Into<String>, payload: Bytes) -> Self {
        Self {
            event_id: EventId::new(),
            topic,
            timestamp: Utc::now(),
            sequence: 0,
            event_type: event_type.into(),
            payload,
            payload_encoding: PayloadEncoding::Json,
            source: None,
            trace_id: None,
            ttl_ms: None,
        }
    }

    /// Return payload size in bytes.
    ///
    /// This is the size of the raw `Bytes` buffer, not the serialized
    /// envelope. Use [`is_payload_too_large()`](Self::is_payload_too_large)
    /// to check against the 64 KB limit.
    pub fn payload_size(&self) -> usize {
        self.payload.len()
    }

    /// Check if the payload exceeds the maximum allowed size (64 KB = 65 536 bytes).
    ///
    /// This is checked at ingestion (REST API and WebSocket handler).
    /// Oversized events are rejected with a 413 status.
    pub fn is_payload_too_large(&self) -> bool {
        self.payload.len() > 65_536
    }
}

// ─── Topic Pattern ───────────────────────────────────────────────────

/// Topic pattern used when subscribing to events.
///
/// Subscriptions can match topics in three ways, from most specific
/// to most general:
///
/// | Variant  | Example            | Matches                                  |
/// |----------|--------------------|------------------------------------------|
/// | `Exact`  | `orders/created`   | Only `orders/created`                    |
/// | `Prefix` | `orders/`          | `orders/created`, `orders/deleted`, etc. |
/// | `Glob`   | `*/created`        | `orders/created`, `users/created`, etc.  |
///
/// The [`parse()`](Self::parse) constructor automatically selects the
/// right variant based on the input string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TopicPattern {
    /// Exact match — the topic must be identical to the path.
    Exact(TopicPath),
    /// Prefix match — the topic must start with this prefix.
    Prefix(SmolStr),
    /// Glob match — `*` matches one segment, `**` matches all remaining.
    Glob(SmolStr),
}

impl TopicPattern {
    /// Test whether a concrete [`TopicPath`] matches this pattern.
    ///
    /// # Arguments
    ///
    /// * `topic` — The concrete topic to test against.
    ///
    /// # Returns
    ///
    /// `true` if the topic matches this pattern.
    pub fn matches(&self, topic: &TopicPath) -> bool {
        match self {
            TopicPattern::Exact(path) => path == topic,
            TopicPattern::Prefix(prefix) => topic.as_str().starts_with(prefix.as_str()),
            TopicPattern::Glob(pattern) => glob_match(pattern.as_str(), topic.as_str()),
        }
    }

    /// Parse a string into the most appropriate `TopicPattern` variant.
    ///
    /// **Rules:**
    /// - Contains `*` → [`Glob`](TopicPattern::Glob)
    /// - Ends with `/` → [`Prefix`](TopicPattern::Prefix)
    /// - Otherwise → [`Exact`](TopicPattern::Exact)
    ///
    /// # Examples
    ///
    /// ```
    /// use realtime_core::TopicPattern;
    /// assert!(matches!(TopicPattern::parse("orders/*"), TopicPattern::Glob(_)));
    /// assert!(matches!(TopicPattern::parse("orders/"), TopicPattern::Prefix(_)));
    /// assert!(matches!(TopicPattern::parse("orders/created"), TopicPattern::Exact(_)));
    /// ```
    pub fn parse(s: &str) -> Self {
        if s.contains('*') {
            TopicPattern::Glob(SmolStr::new(s))
        } else if s.ends_with('/') {
            TopicPattern::Prefix(SmolStr::new(s))
        } else {
            TopicPattern::Exact(TopicPath::new(s))
        }
    }

    /// Return the pattern as a string slice, regardless of variant.
    pub fn as_str(&self) -> &str {
        match self {
            TopicPattern::Exact(path) => path.as_str(),
            TopicPattern::Prefix(s) => s.as_str(),
            TopicPattern::Glob(s) => s.as_str(),
        }
    }
}

impl fmt::Display for TopicPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Simple glob matching for topic paths.
///
/// Rules:
/// - `*` matches exactly one segment (e.g. `orders/*` matches `orders/created`
///   but not `orders/created/extra`).
/// - `**` matches all remaining segments (e.g. `orders/**` matches everything
///   under `orders/`).
/// - Literal segments must match exactly.
///
/// # Arguments
///
/// * `pattern` — The glob pattern, where segments are separated by `/`.
/// * `topic` — The concrete topic path to match against.
fn glob_match(pattern: &str, topic: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('/').collect();
    let topic_parts: Vec<&str> = topic.split('/').collect();

    // "**" alone matches everything
    if pat_parts.len() == 1 && pat_parts[0] == "**" {
        return true;
    }

    // Walk segments, handling trailing "**"
    for (i, p) in pat_parts.iter().enumerate() {
        if *p == "**" {
            // "**" at any position matches all remaining segments
            return true;
        }
        // If we've run out of topic segments, no match
        if i >= topic_parts.len() {
            return false;
        }
        if *p != "*" && *p != topic_parts[i] {
            return false;
        }
    }

    // All pattern parts matched; topic must not have extra segments
    pat_parts.len() == topic_parts.len()
}

// ─── Subscription ────────────────────────────────────────────────────

/// Overflow policy for per-connection send queues.
///
/// When a client's outbound channel is full (because it reads too slowly),
/// one of these policies is applied to handle backpressure:
///
/// | Policy         | Behaviour                                           |
/// |----------------|-----------------------------------------------------|
/// | `DropOldest`   | Remove the oldest queued event to make room.        |
/// | `DropNewest`   | Discard the new event (current default).             |
/// | `Disconnect`   | Force-close the slow connection.                     |
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OverflowPolicy {
    /// Drop the oldest queued event to make room for the new one.
    DropOldest,
    /// Drop the incoming event (keep existing queue intact). **Default.**
    DropNewest,
    /// Force-disconnect the slow client.
    Disconnect,
}

impl Default for OverflowPolicy {
    fn default() -> Self {
        Self::DropNewest
    }
}

/// Per-subscription configuration sent by the client at subscribe time.
///
/// All fields are optional with sensible defaults, so a minimal
/// `SUBSCRIBE` message needs no config at all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubConfig {
    /// How to handle send-queue overflow. Default: [`DropNewest`](OverflowPolicy::DropNewest).
    #[serde(default)]
    pub overflow: OverflowPolicy,
    /// Optional events-per-second cap for this subscription.
    pub rate_limit: Option<u32>,
    /// If set, the server replays events with `sequence > resume_from`.
    /// Used for client reconnection / catch-up.
    pub resume_from: Option<u64>,
}

impl Default for SubConfig {
    fn default() -> Self {
        Self {
            overflow: OverflowPolicy::DropNewest,
            rate_limit: None,
            resume_from: None,
        }
    }
}

/// A live subscription binding a connection to a topic pattern.
///
/// Created when a client sends `SUBSCRIBE`. Stored in the engine's
/// `SubscriptionRegistry` and used
/// by the `FilterIndex` to efficiently match incoming events.
#[derive(Debug, Clone)]
pub struct Subscription {
    pub sub_id: SubscriptionId,
    pub conn_id: ConnectionId,
    pub topic: TopicPattern,
    pub filter: Option<crate::filter::FilterExpr>,
    pub config: SubConfig,
}

// ─── Connection Metadata ─────────────────────────────────────────────

/// Metadata tracked for each active WebSocket connection.
///
/// Stored in the gateway's `ConnectionManager` for the lifetime of
/// the connection. Used for authorization checks, logging, and admin
/// introspection.
#[derive(Debug, Clone)]
pub struct ConnectionMeta {
    /// Unique connection identifier.
    pub conn_id: ConnectionId,
    /// Remote IP:port of the client.
    pub peer_addr: SocketAddr,
    /// When the WebSocket handshake completed.
    pub connected_at: DateTime<Utc>,
    /// Subject claim from the auth token (if authenticated).
    pub user_id: Option<String>,
    /// Full decoded auth claims (if authenticated).
    pub claims: Option<AuthClaims>,
}

/// Authentication claims extracted from a client's token.
///
/// Decoded by an [`AuthProvider`](crate::traits::AuthProvider) during the
/// `AUTH` handshake. Controls which namespaces the client can subscribe to
/// or publish on.
///
/// # Authorization model
///
/// - `can_subscribe` / `can_publish` are global toggles.
/// - `namespaces` restricts access to specific topic namespaces.
///   An empty list means "all namespaces allowed". A `"*"` entry
///   also means unrestricted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthClaims {
    pub sub: String,
    pub namespaces: Vec<String>,
    pub can_publish: bool,
    pub can_subscribe: bool,
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl AuthClaims {
    /// Check if these claims allow subscribing to the given topic pattern.
    ///
    /// Returns `false` if `can_subscribe` is `false`, or if the topic's
    /// namespace is not in the allowed `namespaces` list.
    pub fn can_subscribe_to(&self, topic: &TopicPattern) -> bool {
        if !self.can_subscribe {
            return false;
        }
        if self.namespaces.is_empty() {
            return true; // no restrictions
        }
        let topic_ns = match topic {
            TopicPattern::Exact(p) => p.namespace(),
            TopicPattern::Prefix(p) => p.split('/').next().unwrap_or(""),
            TopicPattern::Glob(p) => {
                if p.as_str() == "**" {
                    return self.namespaces.contains(&"*".to_string());
                }
                p.split('/').next().unwrap_or("")
            }
        };
        self.namespaces.iter().any(|ns| ns == "*" || ns == topic_ns)
    }

    /// Check if these claims allow publishing to the given topic.
    ///
    /// Returns `false` if `can_publish` is `false`, or if the topic's
    /// namespace is not in the allowed list.
    pub fn can_publish_to(&self, topic: &TopicPath) -> bool {
        if !self.can_publish {
            return false;
        }
        if self.namespaces.is_empty() {
            return true;
        }
        let ns = topic.namespace();
        self.namespaces.iter().any(|n| n == "*" || n == ns)
    }
}

/// Receipt returned after a successful publish operation.
///
/// Contains the assigned `event_id`, the topic-scoped `sequence` number,
/// and whether the event reached the event bus successfully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishReceipt {
    pub event_id: EventId,
    pub sequence: u64,
    pub delivered_to_bus: bool,
}

/// Context passed to [`AuthProvider::verify()`](crate::traits::AuthProvider::verify)
/// alongside the token string.
///
/// Allows auth providers to make decisions based on transport type
/// or client IP (e.g., rate limiting by IP).
#[derive(Debug, Clone)]
pub struct AuthContext {
    pub peer_addr: SocketAddr,
    pub transport: String,
}

// ─── Dispatch types ──────────────────────────────────────────────────

/// A batch of events destined for specific connections on a gateway node.
///
/// The `Arc<EventEnvelope>` is shared across all target connections,
/// achieving zero-copy fan-out. The router produces one `DispatchBatch`
/// per event, listing every connection that should receive it.
#[derive(Debug, Clone)]
pub struct DispatchBatch {
    /// The event to deliver (reference-counted, zero-copy across connections).
    pub event: std::sync::Arc<EventEnvelope>,
    /// Connection IDs that should receive this event.
    pub conn_ids: Vec<ConnectionId>,
}

// ─── Frame encoder ───────────────────────────────────────────────────

/// Wire encoding for WebSocket frames sent to clients.
///
/// Currently only JSON is used; MsgPack support is prepared
/// for future binary-optimized transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameEncoding {
    /// JSON text frames (current default).
    Json,
    /// MessagePack binary frames (reserved for future use).
    MsgPack,
}

impl Default for FrameEncoding {
    fn default() -> Self {
        Self::Json
    }
}

// ─── Bytes serde helper ─────────────────────────────────────────────

/// Custom serde helper for the `Bytes` payload field on [`EventEnvelope`].
///
/// ## Serialization strategy
///
/// When serializing, we first attempt to parse the bytes as JSON. If the
/// payload *is* valid JSON, we inline it directly into the parent JSON
/// object (no base64 wrapping). Otherwise, we fall back to raw byte
/// serialization.
///
/// This ensures that JSON payloads look natural in WebSocket frames:
/// ```json
/// { "payload": { "title": "Buy milk" } }
/// ```
/// instead of:
/// ```json
/// { "payload": "eyJ0aXRsZSI6ICJCdXkgbWlsayJ9" }
/// ```
mod bytes_serde {
    use bytes::Bytes;
    use serde::{self, Deserialize, Deserializer, Serialize, Serializer};

    /// Serialize `Bytes` — inlines valid JSON, otherwise raw bytes.
    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // Try to serialize as raw JSON value if it looks like JSON.
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(bytes) {
            val.serialize(serializer)
        } else {
            serializer.serialize_bytes(bytes)
        }
    }

    /// Deserialize into `Bytes` — parses any JSON value then re-serializes to bytes.
    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        let b = serde_json::to_vec(&v).map_err(serde::de::Error::custom)?;
        Ok(Bytes::from(b))
    }
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_id_generation() {
        let id1 = EventId::new();
        let id2 = EventId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_topic_path_parts() {
        let topic = TopicPath::new("orders/created");
        assert_eq!(topic.namespace(), "orders");
        assert_eq!(topic.event_type_part(), "created");
    }

    #[test]
    fn test_topic_pattern_exact() {
        let pattern = TopicPattern::Exact(TopicPath::new("orders/created"));
        assert!(pattern.matches(&TopicPath::new("orders/created")));
        assert!(!pattern.matches(&TopicPath::new("orders/updated")));
    }

    #[test]
    fn test_topic_pattern_prefix() {
        let pattern = TopicPattern::Prefix(SmolStr::new("orders/"));
        assert!(pattern.matches(&TopicPath::new("orders/created")));
        assert!(pattern.matches(&TopicPath::new("orders/deleted")));
        assert!(!pattern.matches(&TopicPath::new("users/created")));
    }

    #[test]
    fn test_topic_pattern_glob() {
        let pattern = TopicPattern::Glob(SmolStr::new("orders/*"));
        assert!(pattern.matches(&TopicPath::new("orders/created")));
        assert!(pattern.matches(&TopicPath::new("orders/anything")));
        assert!(!pattern.matches(&TopicPath::new("users/anything")));

        let pattern2 = TopicPattern::Glob(SmolStr::new("*/created"));
        assert!(pattern2.matches(&TopicPath::new("orders/created")));
        assert!(pattern2.matches(&TopicPath::new("users/created")));
        assert!(!pattern2.matches(&TopicPath::new("users/deleted")));
    }

    #[test]
    fn test_topic_pattern_parse() {
        assert!(matches!(TopicPattern::parse("orders/created"), TopicPattern::Exact(_)));
        assert!(matches!(TopicPattern::parse("orders/"), TopicPattern::Prefix(_)));
        assert!(matches!(TopicPattern::parse("orders/*"), TopicPattern::Glob(_)));
    }

    #[test]
    fn test_event_envelope_creation() {
        let payload = Bytes::from(r#"{"key":"value"}"#);
        let event = EventEnvelope::new(
            TopicPath::new("test/event"),
            "created",
            payload.clone(),
        );
        assert_eq!(event.topic, TopicPath::new("test/event"));
        assert_eq!(event.event_type, "created");
        assert!(!event.is_payload_too_large());
    }

    #[test]
    fn test_payload_too_large() {
        let payload = Bytes::from(vec![0u8; 70_000]);
        let event = EventEnvelope::new(TopicPath::new("test"), "test", payload);
        assert!(event.is_payload_too_large());
    }

    #[test]
    fn test_auth_claims_subscribe() {
        let claims = AuthClaims {
            sub: "user1".to_string(),
            namespaces: vec!["orders".to_string(), "users".to_string()],
            can_publish: false,
            can_subscribe: true,
            metadata: HashMap::new(),
        };
        assert!(claims.can_subscribe_to(&TopicPattern::Exact(TopicPath::new("orders/created"))));
        assert!(!claims.can_subscribe_to(&TopicPattern::Exact(TopicPath::new("admin/settings"))));
    }
}

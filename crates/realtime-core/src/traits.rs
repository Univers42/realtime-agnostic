/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   traits.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:11:43 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:11:44 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Trait definitions that form the **extension points** of the engine.
//!
//! Every component that can be swapped (event bus, auth provider, database
//! producer, transport layer) is defined here as a trait. Concrete
//! implementations live in their own crates (e.g., `realtime-bus-inprocess`,
//! `realtime-db-postgres`), keeping the core free of any specific dependency.

use async_trait::async_trait;
use std::net::SocketAddr;

use crate::error::Result;
use crate::types::*;

// ─── Event Bus Traits ────────────────────────────────────────────────

/// Core pub/sub event bus abstraction.
///
/// The event bus is the backbone that connects **producers** (database CDC,
/// REST API, WebSocket publishes) with the **router**. All events flow
/// through the bus.
///
/// ## Implementations
///
/// | Crate                    | Backend        | Use case            |
/// |--------------------------|----------------|---------------------|
/// | `realtime-bus-inprocess`  | `tokio::broadcast` | Single-node, dev |
/// | *(future)*               | Redis Streams  | Multi-node cluster  |
/// | *(future)*               | NATS JetStream | Cloud-native        |
///
/// ## Lifetime
///
/// A single `EventBus` instance is created at server startup and shared
/// (via `Arc`) across all producers and the router subscriber.
#[async_trait]
pub trait EventBus: Send + Sync + 'static {
    /// Create a publisher handle for sending events into the bus.
    ///
    /// Each producer (database adapter, REST API, WebSocket handler) gets
    /// its own publisher to avoid contention.
    async fn publisher(&self) -> Result<Box<dyn EventBusPublisher>>;

    /// Create a subscriber that receives events matching `topic_pattern`.
    ///
    /// # Arguments
    ///
    /// * `topic_pattern` — A topic pattern string (e.g. `"*"` for all topics).
    ///   The engine creates one global subscriber with `"*"`.
    async fn subscriber(&self, topic_pattern: &str) -> Result<Box<dyn EventBusSubscriber>>;

    /// Health check the underlying bus connection.
    async fn health_check(&self) -> Result<()>;

    /// Shut down the bus gracefully, flushing any pending messages.
    async fn shutdown(&self) -> Result<()>;
}

/// Publisher side of the event bus — sends events into the bus.
///
/// Obtained from [`EventBus::publisher()`]. Thread-safe (`Send + Sync`)
/// but typically not shared — each producer task gets its own handle.
#[async_trait]
pub trait EventBusPublisher: Send + Sync {
    /// Publish a single event to the given topic.
    ///
    /// # Arguments
    ///
    /// * `topic` — The topic string to publish on.
    /// * `event` — The fully-formed event envelope.
    ///
    /// # Returns
    ///
    /// A [`PublishReceipt`] confirming the event was accepted by the bus.
    async fn publish(&self, topic: &str, event: &EventEnvelope) -> Result<PublishReceipt>;

    /// Publish multiple events atomically.
    ///
    /// # Arguments
    ///
    /// * `events` — Slice of `(topic, envelope)` pairs.
    async fn publish_batch(
        &self,
        events: &[(String, EventEnvelope)],
    ) -> Result<Vec<PublishReceipt>>;
}

/// Subscriber side of the event bus — receives events from the bus.
///
/// The engine creates exactly one subscriber (topic `"*"`) and feeds
/// every received event into the router for fan-out.
#[async_trait]
pub trait EventBusSubscriber: Send + Sync {
    /// Block until the next event is available, or return `None` if the
    /// bus has been shut down.
    async fn next_event(&mut self) -> Option<EventEnvelope>;

    /// Acknowledge successful processing of an event (for buses that
    /// support at-least-once delivery).
    async fn ack(&self, event_id: &EventId) -> Result<()>;

    /// Negative acknowledge — request redelivery of the event.
    async fn nack(&self, event_id: &EventId) -> Result<()>;
}

// ─── Transport Traits ────────────────────────────────────────────────

/// Transport server that accepts incoming client connections.
///
/// Currently the only implementation is the Axum-based WebSocket server
/// in `realtime-gateway`; this trait exists so an alternative transport
/// (e.g. raw TCP, QUIC) could be plugged in.
#[async_trait]
pub trait TransportServer: Send + Sync + 'static {
    /// Bind the server to a network address and begin listening.
    async fn bind(&self, addr: SocketAddr) -> Result<()>;

    /// Accept the next incoming connection, returning the transport
    /// handle and connection metadata.
    async fn accept(&self) -> Result<(Box<dyn TransportConnection>, ConnectionMeta)>;
}

/// A single bidirectional client connection (transport-agnostic).
///
/// Represents one WebSocket (or future TCP/QUIC) connection. The gateway
/// splits each connection into a reader task and a writer channel.
#[async_trait]
pub trait TransportConnection: Send + Sync {
    /// Receive the next message from the client, or `None` on disconnect.
    async fn recv_message(&mut self) -> Result<Option<ClientMessage>>;

    /// Send a message to the client.
    async fn send_message(&mut self, msg: ServerMessage) -> Result<()>;

    /// Gracefully close the connection with a close code and reason.
    async fn close(&mut self, code: u16, reason: &str) -> Result<()>;

    /// Return the peer's socket address.
    fn peer_addr(&self) -> SocketAddr;
}

// ─── Auth Traits ─────────────────────────────────────────────────────

/// Authentication provider — verifies tokens and authorizes operations.
///
/// ## Implementations
///
/// | Module     | Strategy   | Use case                  |
/// |------------|------------|---------------------------|
/// | `jwt`      | HS256 JWT  | Production with secrets   |
/// | `noauth`   | Allow all  | Development / testing     |
///
/// Custom providers can be created by implementing this trait and
/// registering the provider at server construction time.
#[async_trait]
pub trait AuthProvider: Send + Sync + 'static {
    /// Verify a bearer token and return decoded [`AuthClaims`].
    ///
    /// # Arguments
    ///
    /// * `token` — The raw token string from the `AUTH` message.
    /// * `context` — Peer address and transport metadata.
    async fn verify(&self, token: &str, context: &AuthContext) -> Result<AuthClaims>;

    /// Authorize a subscribe request against the client's claims.
    ///
    /// Called after `verify()` succeeds, before the subscription is created.
    async fn authorize_subscribe(
        &self,
        claims: &AuthClaims,
        topic: &TopicPattern,
    ) -> Result<()>;

    /// Authorize a publish request against the client's claims.
    ///
    /// Called before the event enters the bus.
    async fn authorize_publish(
        &self,
        claims: &AuthClaims,
        topic: &TopicPath,
    ) -> Result<()>;
}

// ─── Database Change Producer Trait ──────────────────────────────────

/// Database change data capture (CDC) producer.
///
/// Implementations watch a database for changes and translate them into
/// [`EventEnvelope`]s that flow through the event bus. Each database
/// adapter crate provides exactly one implementation of this trait.
///
/// ## Lifecycle
///
/// 1. Server calls [`start()`](Self::start) → spawns internal listener task.
/// 2. Returned [`EventStream`] yields envelopes whenever the DB changes.
/// 3. Server calls [`stop()`](Self::stop) during graceful shutdown.
#[async_trait]
pub trait DatabaseProducer: Send + Sync + 'static {
    /// Start watching for changes. Returns an event stream.
    ///
    /// The implementation should connect to the database, configure
    /// a change listener (e.g., PG `LISTEN` or Mongo change stream),
    /// and return a stream that yields events as they arrive.
    async fn start(&self) -> Result<Box<dyn EventStream>>;

    /// Stop watching for changes and release database resources.
    async fn stop(&self) -> Result<()>;

    /// Check that the database connection is healthy.
    async fn health_check(&self) -> Result<()>;

    /// Human-readable name of this producer (e.g. `"postgresql"`, `"mongodb"`).
    fn name(&self) -> &str;
}

/// An async stream of [`EventEnvelope`]s from any event source.
///
/// Returned by [`DatabaseProducer::start()`]. The engine polls this
/// stream in a loop, forwarding each event to the event bus.
#[async_trait]
pub trait EventStream: Send + Sync {
    /// Yield the next event, or `None` if the stream has ended.
    async fn next_event(&mut self) -> Option<EventEnvelope>;
}

// ─── Producer Factory (Adapter Pattern) ──────────────────────────────

/// Factory trait for creating database producers from generic config.
///
/// Implement this trait to register a new database adapter. The factory
/// receives a JSON config blob and returns a configured `DatabaseProducer`.
///
/// # Example
/// ```rust,ignore
/// struct PostgresFactory;
/// impl ProducerFactory for PostgresFactory {
///     fn name(&self) -> &str { "postgresql" }
///     fn create(&self, config: serde_json::Value) -> Result<Box<dyn DatabaseProducer>> {
///         let pg_config: PostgresConfig = serde_json::from_value(config)?;
///         Ok(Box::new(PostgresProducer::new(pg_config)))
///     }
/// }
/// ```
pub trait ProducerFactory: Send + Sync + 'static {
    /// Adapter name (e.g. "postgresql", "mongodb", "mysql", "redis").
    fn name(&self) -> &str;

    /// Create a producer from a generic JSON config.
    fn create(&self, config: serde_json::Value) -> Result<Box<dyn DatabaseProducer>>;
}

// ─── Client/Server Messages (Protocol) ──────────────────────────────
// These are defined in protocol.rs but re-exported here for trait usage.

use crate::protocol::{ClientMessage, ServerMessage};

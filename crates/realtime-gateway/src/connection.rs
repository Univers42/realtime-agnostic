/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   connection.rs                                      :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:25 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:14:21 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Connection manager — tracks all active WebSocket connections.
//!
//! Each connected client gets a [`ConnectionState`] containing its
//! metadata, per-connection send channel, and overflow policy.
//! The manager is backed by [`DashMap`] for lock-free concurrent access.
//!
//! ## Per-connection isolation
//!
//! Every connection has its own bounded `mpsc` channel. The fan-out
//! task writes events into the channel; a per-connection writer task
//! reads from it and sends WebSocket frames. If a client reads too
//! slowly, backpressure is applied according to its [`OverflowPolicy`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use dashmap::DashMap;
use realtime_core::{ConnectionId, ConnectionMeta, EventEnvelope, OverflowPolicy};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// State for a single active WebSocket connection.
///
/// Holds the metadata (peer address, auth claims), the send channel
/// for delivering events, and the overflow policy for backpressure.
pub struct ConnectionState {
    /// Connection metadata (peer addr, timestamps, auth claims).
    pub meta: ConnectionMeta,
    /// Bounded sender for delivering events to this connection's writer task.
    pub send_tx: mpsc::Sender<Arc<EventEnvelope>>,
    /// What to do when the send queue is full.
    pub overflow_policy: OverflowPolicy,
}

/// Manages all active connections on this gateway node.
///
/// Thread-safe with [`DashMap`] sharding for lock-free reads. The
/// connection manager is shared (via `Arc`) across the WebSocket
/// handler, fan-out pool, and REST API.
pub struct ConnectionManager {
    /// All active connections: conn_id → state.
    connections: DashMap<ConnectionId, ConnectionState>,

    /// Monotonically increasing connection ID counter (atomic).
    next_conn_id: AtomicU64,

    /// Default send queue capacity per connection (number of events).
    send_queue_capacity: usize,
}

impl ConnectionManager {
    /// Create a new connection manager.
    ///
    /// # Arguments
    ///
    /// * `send_queue_capacity` — Number of events each connection's
    ///   outbound channel can hold before triggering overflow policy.
    ///   A typical value is 256.
    pub fn new(send_queue_capacity: usize) -> Self {
        Self {
            connections: DashMap::new(),
            next_conn_id: AtomicU64::new(1),
            send_queue_capacity,
        }
    }

    /// Allocate the next unique connection ID (atomic increment).
    pub fn next_connection_id(&self) -> ConnectionId {
        ConnectionId(self.next_conn_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Register a new connection and return its send-channel receiver.
    ///
    /// The caller (WebSocket handler) spawns a writer task that reads
    /// from the returned `Receiver` and sends frames to the client.
    ///
    /// # Arguments
    ///
    /// * `meta` — Connection metadata (must include the allocated `conn_id`).
    /// * `overflow_policy` — Backpressure strategy for this connection.
    ///
    /// # Returns
    ///
    /// `(ConnectionId, mpsc::Receiver)` — the receiver for the writer task.
    pub fn register(
        &self,
        meta: ConnectionMeta,
        overflow_policy: OverflowPolicy,
    ) -> (ConnectionId, mpsc::Receiver<Arc<EventEnvelope>>) {
        let conn_id = meta.conn_id;
        let (send_tx, send_rx) = mpsc::channel(self.send_queue_capacity);

        let state = ConnectionState {
            meta,
            send_tx,
            overflow_policy,
        };

        self.connections.insert(conn_id, state);
        info!(conn_id = %conn_id, "Connection registered");

        (conn_id, send_rx)
    }

    /// Remove a connection from the manager (called on disconnect).
    pub fn remove(&self, conn_id: ConnectionId) {
        if self.connections.remove(&conn_id).is_some() {
            info!(conn_id = %conn_id, "Connection removed");
        }
    }

    /// Attempt to enqueue an event for a connection without blocking.
    ///
    /// Applies the connection's overflow policy if the queue is full:
    /// - `DropNewest` → silently discard the event.
    /// - `DropOldest` → (approximated) discard the event.
    /// - `Disconnect` → signal that the connection should be closed.
    ///
    /// Returns [`SendResult::ConnectionGone`] if the connection no longer exists.
    pub fn try_send(
        &self,
        conn_id: ConnectionId,
        event: Arc<EventEnvelope>,
    ) -> SendResult {
        if let Some(state) = self.connections.get(&conn_id) {
            match state.send_tx.try_send(event) {
                Ok(()) => SendResult::Sent,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    match state.overflow_policy {
                        OverflowPolicy::DropNewest => {
                            debug!(conn_id = %conn_id, "Send queue full, dropping newest event");
                            SendResult::DroppedNewest
                        }
                        OverflowPolicy::DropOldest => {
                            // We can't easily drop oldest from mpsc, so skip this event
                            debug!(conn_id = %conn_id, "Send queue full (drop-oldest not ideal with mpsc), dropping newest");
                            SendResult::DroppedOldest
                        }
                        OverflowPolicy::Disconnect => {
                            warn!(conn_id = %conn_id, "Send queue full, disconnecting");
                            SendResult::Disconnect
                        }
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    SendResult::ConnectionGone
                }
            }
        } else {
            SendResult::ConnectionGone
        }
    }

    /// Return the total number of active connections.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Check whether a connection with the given ID is currently active.
    pub fn has_connection(&self, conn_id: ConnectionId) -> bool {
        self.connections.contains_key(&conn_id)
    }

    /// Return a clone of the connection metadata, or `None` if not found.
    pub fn get_meta(&self, conn_id: ConnectionId) -> Option<ConnectionMeta> {
        self.connections.get(&conn_id).map(|s| s.meta.clone())
    }

    /// Return a snapshot of all active connection IDs.
    pub fn all_connection_ids(&self) -> Vec<ConnectionId> {
        self.connections.iter().map(|e| *e.key()).collect()
    }
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new(256)
    }
}

/// Result of attempting to send an event to a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    /// Event was successfully enqueued.
    Sent,
    /// Queue was full; event was dropped (DropNewest policy).
    DroppedNewest,
    /// Queue was full; approximated drop-oldest behaviour.
    DroppedOldest,
    /// Queue was full; connection should be terminated (Disconnect policy).
    Disconnect,
    /// Connection no longer exists in the manager.
    ConnectionGone,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    

    fn make_meta(conn_id: ConnectionId) -> ConnectionMeta {
        ConnectionMeta {
            conn_id,
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            connected_at: Utc::now(),
            user_id: None,
            claims: None,
        }
    }

    #[test]
    fn test_connection_registration() {
        let mgr = ConnectionManager::new(256);
        let conn_id = mgr.next_connection_id();
        let meta = make_meta(conn_id);

        let (id, _rx) = mgr.register(meta, OverflowPolicy::DropNewest);
        assert_eq!(id, conn_id);
        assert_eq!(mgr.connection_count(), 1);
        assert!(mgr.has_connection(conn_id));
    }

    #[test]
    fn test_connection_removal() {
        let mgr = ConnectionManager::new(256);
        let conn_id = mgr.next_connection_id();
        let meta = make_meta(conn_id);

        mgr.register(meta, OverflowPolicy::DropNewest);
        mgr.remove(conn_id);
        assert_eq!(mgr.connection_count(), 0);
        assert!(!mgr.has_connection(conn_id));
    }

    #[tokio::test]
    async fn test_try_send() {
        let mgr = ConnectionManager::new(10);
        let conn_id = mgr.next_connection_id();
        let meta = make_meta(conn_id);
        let (_, mut rx) = mgr.register(meta, OverflowPolicy::DropNewest);

        let event = Arc::new(EventEnvelope::new(
            realtime_core::TopicPath::new("test"),
            "test",
            bytes::Bytes::from("{}"),
        ));

        let result = mgr.try_send(conn_id, event);
        assert_eq!(result, SendResult::Sent);

        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type, "test");
    }

    #[test]
    fn test_send_to_nonexistent() {
        let mgr = ConnectionManager::new(10);
        let event = Arc::new(EventEnvelope::new(
            realtime_core::TopicPath::new("test"),
            "test",
            bytes::Bytes::from("{}"),
        ));

        let result = mgr.try_send(ConnectionId(999), event);
        assert_eq!(result, SendResult::ConnectionGone);
    }
}

/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   fanout.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:28 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Fan-out worker pool — bridges the router's dispatch channel to
//! per-connection send queues.
//!
//! The pool spawns N worker tasks (default: CPU count) that compete
//! to read from a shared `mpsc` channel. Each [`LocalDispatch`] is
//! forwarded to the target connection via [`ConnectionManager::try_send()`].
//!
//! ## Backpressure handling
//!
//! - `Sent` → nominal path, no action.
//! - `DroppedNewest` / `DroppedOldest` → logged at DEBUG level.
//! - `Disconnect` → connection is removed from the manager.
//! - `ConnectionGone` → logged at DEBUG level (stale dispatch).

use std::sync::Arc;

use realtime_engine::router::LocalDispatch;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::connection::{ConnectionManager, SendResult};

/// Fan-out worker pool that delivers events from the router to connections.
///
/// Internally spawns `worker_count` Tokio tasks that share a single
/// dispatch channel via `Arc<Mutex<Receiver>>`.
pub struct FanOutWorkerPool {
    /// Shared reference to the connection manager.
    conn_manager: Arc<ConnectionManager>,
    /// Number of worker tasks to spawn.
    worker_count: usize,
}

impl FanOutWorkerPool {
    /// Create a new pool (does not start workers yet).
    ///
    /// # Arguments
    ///
    /// * `conn_manager` — Shared connection manager.
    /// * `worker_count` — Number of concurrent fan-out tasks.
    pub fn new(conn_manager: Arc<ConnectionManager>, worker_count: usize) -> Self {
        Self {
            conn_manager,
            worker_count,
        }
    }

    /// Spawn worker tasks and return a sender for dispatch instructions.
    ///
    /// The returned `mpsc::Sender<LocalDispatch>` is given to the
    /// `EventRouter` which writes one dispatch per matching subscription.
    /// The workers consume these dispatches and forward events to
    /// the appropriate connection send queues.
    ///
    /// The channel capacity is 65 536 to absorb traffic spikes.
    pub fn start(&self) -> mpsc::Sender<LocalDispatch> {
        let (tx, rx) = mpsc::channel::<LocalDispatch>(65536);

        // Distribute work across multiple worker tasks
        let shared_rx = Arc::new(tokio::sync::Mutex::new(rx));

        for worker_id in 0..self.worker_count {
            let conn_manager = Arc::clone(&self.conn_manager);
            let rx = Arc::clone(&shared_rx);

            tokio::spawn(async move {
                loop {
                    let dispatch = {
                        let mut guard = rx.lock().await;
                        guard.recv().await
                    };

                    match dispatch {
                        Some(LocalDispatch { conn_id, sub_id: _, event }) => {
                            let result = conn_manager.try_send(conn_id, event);
                            match result {
                                SendResult::Sent => {}
                                SendResult::DroppedNewest | SendResult::DroppedOldest => {
                                    debug!(
                                        worker = worker_id,
                                        conn_id = %conn_id,
                                        "Event dropped due to overflow"
                                    );
                                }
                                SendResult::Disconnect => {
                                    warn!(
                                        worker = worker_id,
                                        conn_id = %conn_id,
                                        "Disconnecting slow consumer"
                                    );
                                    conn_manager.remove(conn_id);
                                }
                                SendResult::ConnectionGone => {
                                    debug!(
                                        worker = worker_id,
                                        conn_id = %conn_id,
                                        "Connection gone during fan-out"
                                    );
                                }
                            }
                        }
                        None => {
                            debug!(worker = worker_id, "Fan-out worker exiting (channel closed)");
                            break;
                        }
                    }
                }
            });
        }

        tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use chrono::Utc;
    use realtime_core::{ConnectionMeta, EventEnvelope, OverflowPolicy, SubscriptionId, TopicPath};
    use smol_str::SmolStr;

    #[tokio::test]
    async fn test_fanout_delivery() {
        let conn_manager = Arc::new(ConnectionManager::new(256));
        let pool = FanOutWorkerPool::new(Arc::clone(&conn_manager), 2);
        let dispatch_tx = pool.start();

        // Register a connection
        let conn_id = conn_manager.next_connection_id();
        let meta = ConnectionMeta {
            conn_id,
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            connected_at: Utc::now(),
            user_id: None,
            claims: None,
        };
        let (_, mut rx) = conn_manager.register(meta, OverflowPolicy::DropNewest);

        // Send a dispatch
        let event = Arc::new(EventEnvelope::new(
            TopicPath::new("test"),
            "test",
            Bytes::from("{}"),
        ));

        dispatch_tx.send(LocalDispatch {
            conn_id,
            sub_id: SubscriptionId(SmolStr::new("sub-1")),
            event,
        }).await.unwrap();

        // Receive on the connection
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            rx.recv(),
        ).await.unwrap().unwrap();

        assert_eq!(received.event_type, "test");
    }
}

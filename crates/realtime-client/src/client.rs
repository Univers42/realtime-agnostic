/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   client.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:11:54 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Async Rust client SDK for the realtime server.
//!
//! Provides [`RealtimeClient`] with:
//! - Automatic exponential-backoff reconnection
//! - Transparent re-subscription after reconnect
//! - Sliding-window event deduplication (last 1 000 IDs)
//! - Builder pattern via [`RealtimeClientBuilder`]
//!
//! ## Quick start
//!
//! ```rust,ignore
//! let client = RealtimeClient::builder("ws://localhost:9090/ws")
//!     .token("my-jwt")
//!     .build()
//!     .await?;
//! let mut events = client.connect().await?;
//! client.subscribe("s1", "orders/*", None).await?;
//! while let Some(e) = events.recv().await {
//!     println!("{:?}", e);
//! }
//! ```

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use realtime_core::{ClientMessage, EventPayload, ServerMessage};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, error, info, warn};


/// Builder for [`RealtimeClient`].
///
/// Uses the builder pattern so callers only set what they need:
///
/// ```rust,ignore
/// RealtimeClientBuilder::new("ws://localhost:9090/ws")
///     .token("my-jwt")
///     .reconnect(true)
///     .max_reconnect_delay(Duration::from_secs(60))
///     .build()
///     .await?;
/// ```
pub struct RealtimeClientBuilder {
    url: String,
    token: String,
    reconnect: bool,
    max_reconnect_delay: Duration,
}

impl RealtimeClientBuilder {
    /// Create a new builder with the WebSocket server URL.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            token: String::new(),
            reconnect: true,
            max_reconnect_delay: Duration::from_secs(30),
        }
    }

    /// Set the authentication token (JWT or opaque string).
    pub fn token(mut self, token: impl Into<String>) -> Self {
        self.token = token.into();
        self
    }

    /// Enable or disable automatic reconnection (default: `true`).
    pub fn reconnect(mut self, enabled: bool) -> Self {
        self.reconnect = enabled;
        self
    }

    /// Set the maximum delay between reconnection attempts.
    pub fn max_reconnect_delay(mut self, delay: Duration) -> Self {
        self.max_reconnect_delay = delay;
        self
    }

    /// Build the client. Does **not** connect yet.
    pub async fn build(self) -> anyhow::Result<RealtimeClient> {
        let client = RealtimeClient {
            url: self.url,
            token: self.token,
            reconnect_enabled: self.reconnect,
            max_reconnect_delay: self.max_reconnect_delay,
            subscriptions: Arc::new(RwLock::new(Vec::new())),
            event_tx: Arc::new(RwLock::new(None)),
            connected: Arc::new(RwLock::new(false)),
            seen_event_ids: Arc::new(Mutex::new(HashSet::new())),
        };

        Ok(client)
    }
}

/// Realtime client SDK with automatic reconnection and deduplication.
///
/// After calling [`connect()`](Self::connect), the client runs its event loop
/// in a background Tokio task. Events arrive on the returned `mpsc::Receiver`.
///
/// Subscriptions are persisted locally so they survive reconnections.
pub struct RealtimeClient {
    url: String,
    token: String,
    reconnect_enabled: bool,
    max_reconnect_delay: Duration,
    subscriptions: Arc<RwLock<Vec<SubscriptionState>>>,
    event_tx: Arc<RwLock<Option<mpsc::Sender<Message>>>>,
    connected: Arc<RwLock<bool>>,
    /// Sliding window for deduplication (last 1000 event IDs).
    seen_event_ids: Arc<Mutex<HashSet<String>>>,
}

/// Internal subscription state tracked for transparent re-subscription.
#[derive(Debug, Clone)]
struct SubscriptionState {
    sub_id: String,
    topic: String,
    filter: Option<serde_json::Value>,
    last_sequence: Option<u64>,
}

impl RealtimeClient {
    /// Shorthand for `RealtimeClientBuilder::new(url)`.
    pub fn builder(url: impl Into<String>) -> RealtimeClientBuilder {
        RealtimeClientBuilder::new(url)
    }

    /// Connect to the server and start the background event loop.
    ///
    /// Returns an `mpsc::Receiver<EventPayload>` that delivers deduplicated events.
    /// On disconnect, the client automatically reconnects with exponential backoff
    /// (if enabled) and re-subscribes to all active topics.
    pub async fn connect(&self) -> anyhow::Result<mpsc::Receiver<EventPayload>> {
        let (event_tx, event_rx) = mpsc::channel::<EventPayload>(1024);
        let url = self.url.clone();
        let token = self.token.clone();
        let subscriptions = Arc::clone(&self.subscriptions);
        let connected = Arc::clone(&self.connected);
        let seen_ids = Arc::clone(&self.seen_event_ids);
        let reconnect_enabled = self.reconnect_enabled;
        let max_delay = self.max_reconnect_delay;
        let ws_tx_holder = Arc::clone(&self.event_tx);

        tokio::spawn(async move {
            let mut attempt = 0u32;

            loop {
                match connect_async(&url).await {
                    Ok((ws_stream, _)) => {
                        attempt = 0;
                        *connected.write().await = true;
                        info!("Connected to {}", url);

                        let (mut write, mut read) = ws_stream.split();

                        // Authenticate
                        let auth_msg = ClientMessage::Auth {
                            token: token.clone(),
                        };
                        let auth_json = serde_json::to_string(&auth_msg).unwrap();
                        if write.send(Message::Text(auth_json.into())).await.is_err() {
                            error!("Failed to send auth message");
                            continue;
                        }

                        // Store writer for sending subscribe messages
                        let (tx, mut rx) = mpsc::channel::<Message>(256);
                        *ws_tx_holder.write().await = Some(tx);

                        // Re-subscribe to all active subscriptions
                        {
                            let subs = subscriptions.read().await;
                            for sub in subs.iter() {
                                let msg = ClientMessage::Subscribe {
                                    sub_id: sub.sub_id.clone(),
                                    topic: sub.topic.clone(),
                                    filter: sub.filter.clone(),
                                    options: sub.last_sequence.map(|seq| {
                                        realtime_core::SubOptions {
                                            overflow: None,
                                            resume_from: Some(seq),
                                            rate_limit: None,
                                        }
                                    }),
                                };
                                let json = serde_json::to_string(&msg).unwrap();
                                let _ = write.send(Message::Text(json.into())).await;
                            }
                        }

                        // Forward messages from internal channel to WebSocket
                        let write = Arc::new(Mutex::new(write));
                        let write_clone = Arc::clone(&write);
                        let forward_task = tokio::spawn(async move {
                            while let Some(msg) = rx.recv().await {
                                let mut w = write_clone.lock().await;
                                if w.send(msg).await.is_err() {
                                    break;
                                }
                            }
                        });

                        // Process incoming messages
                        loop {
                            match read.next().await {
                                Some(Ok(Message::Text(text))) => {
                                    if let Ok(server_msg) =
                                        serde_json::from_str::<ServerMessage>(&text)
                                    {
                                        match server_msg {
                                            ServerMessage::Event { sub_id, event } => {
                                                // Dedup check
                                                let mut seen = seen_ids.lock().await;
                                                if seen.contains(&event.event_id) {
                                                    debug!(
                                                        "Duplicate event {}, skipping",
                                                        event.event_id
                                                    );
                                                    continue;
                                                }
                                                seen.insert(event.event_id.clone());

                                                // Keep sliding window bounded
                                                if seen.len() > 1000 {
                                                    // Simple approach: clear half
                                                    let to_remove: Vec<String> = seen
                                                        .iter()
                                                        .take(500)
                                                        .cloned()
                                                        .collect();
                                                    for id in to_remove {
                                                        seen.remove(&id);
                                                    }
                                                }
                                                drop(seen);

                                                // Update last sequence
                                                {
                                                    let mut subs =
                                                        subscriptions.write().await;
                                                    if let Some(s) = subs
                                                        .iter_mut()
                                                        .find(|s| s.sub_id == sub_id)
                                                    {
                                                        s.last_sequence =
                                                            Some(event.sequence);
                                                    }
                                                }

                                                if event_tx.send(event).await.is_err() {
                                                    info!("Event receiver dropped");
                                                    return;
                                                }
                                            }
                                            ServerMessage::AuthOk { conn_id, .. } => {
                                                info!("Authenticated as {}", conn_id);
                                            }
                                            ServerMessage::Error { code, message } => {
                                                error!(
                                                    "Server error: {} - {}",
                                                    code, message
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                Some(Ok(Message::Close(_))) | None => {
                                    warn!("WebSocket closed");
                                    break;
                                }
                                Some(Err(e)) => {
                                    error!("WebSocket error: {}", e);
                                    break;
                                }
                                _ => {}
                            }
                        }

                        forward_task.abort();
                        *connected.write().await = false;
                    }
                    Err(e) => {
                        error!("Connection failed: {}", e);
                    }
                }

                if !reconnect_enabled {
                    break;
                }

                // Exponential backoff with jitter
                attempt += 1;
                let base_delay = Duration::from_millis(100 * 2u64.pow(attempt.min(10)));
                let jitter = Duration::from_millis(rand::random::<u64>() % 500);
                let delay = (base_delay + jitter).min(max_delay);
                warn!("Reconnecting in {:?} (attempt {})", delay, attempt);
                tokio::time::sleep(delay).await;
            }
        });

        Ok(event_rx)
    }

    /// Subscribe to a topic.
    ///
    /// The subscription is recorded locally so it persists across reconnections.
    /// If the client is currently connected, the subscribe message is sent
    /// immediately.
    ///
    /// # Arguments
    ///
    /// * `sub_id` — Unique subscription identifier (chosen by caller).
    /// * `topic` — Topic pattern (e.g. `"orders/created"`).
    /// * `filter` — Optional server-side filter expression (JSON).
    pub async fn subscribe(
        &self,
        sub_id: impl Into<String>,
        topic: impl Into<String>,
        filter: Option<serde_json::Value>,
    ) -> anyhow::Result<()> {
        let sub_id = sub_id.into();
        let topic = topic.into();

        // Store subscription state for reconnect
        {
            let mut subs = self.subscriptions.write().await;
            subs.push(SubscriptionState {
                sub_id: sub_id.clone(),
                topic: topic.clone(),
                filter: filter.clone(),
                last_sequence: None,
            });
        }

        // Send subscribe message if connected
        if let Some(ref tx) = *self.event_tx.read().await {
            let msg = ClientMessage::Subscribe {
                sub_id,
                topic,
                filter,
                options: None,
            };
            let json = serde_json::to_string(&msg)?;
            tx.send(Message::Text(json.into())).await?;
        }

        Ok(())
    }

    /// Unsubscribe from a subscription by its `sub_id`.
    ///
    /// Removes the local state and sends an `Unsubscribe` message if connected.
    pub async fn unsubscribe(&self, sub_id: &str) -> anyhow::Result<()> {
        {
            let mut subs = self.subscriptions.write().await;
            subs.retain(|s| s.sub_id != sub_id);
        }

        if let Some(ref tx) = *self.event_tx.read().await {
            let msg = ClientMessage::Unsubscribe {
                sub_id: sub_id.to_string(),
            };
            let json = serde_json::to_string(&msg)?;
            tx.send(Message::Text(json.into())).await?;
        }

        Ok(())
    }

    /// Returns `true` if the WebSocket connection is currently active.
    pub async fn is_connected(&self) -> bool {
        *self.connected.read().await
    }
}

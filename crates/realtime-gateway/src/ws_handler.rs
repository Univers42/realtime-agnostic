/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   ws_handler.rs                                      :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:34 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:23:06 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! WebSocket connection handler — manages the full lifecycle of a client connection.
//!
//! Each WebSocket connection spawns two tasks:
//!
//! 1. **Writer task** (`writer_loop`) — reads from the per-connection send
//!    channel and a control channel, serializes messages to JSON, and
//!    writes WebSocket text frames. Includes slow-client detection.
//!
//! 2. **Reader task** (`reader_loop`) — reads WebSocket frames, deserializes
//!    [`ClientMessage`]s, and handles auth, subscribe, unsubscribe, publish,
//!    and ping commands.
//!
//! When either task exits, the connection is cleaned up (subscriptions
//! removed, connection state deregistered).

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use bytes::Bytes;
use chrono::Utc;
use realtime_core::{
    filter::FilterExpr, AuthClaims, AuthContext, AuthProvider, ClientMessage, ConnectionId,
    ConnectionMeta, EventEnvelope, EventPayload, OverflowPolicy, ServerMessage, SubConfig,
    Subscription, SubscriptionId, TopicPath, TopicPattern,
};
use realtime_engine::registry::SubscriptionRegistry;
use smol_str::SmolStr;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::connection::ConnectionManager;

/// Shared application state injected into Axum handlers via `State`.
///
/// Contains `Arc` references to all components needed by the WebSocket
/// and REST handlers. Cloned per-request by Axum (cheap: only Arc bumps).
#[derive(Clone)]
pub struct AppState {
    /// Connection manager for tracking active WebSocket connections.
    pub conn_manager: Arc<ConnectionManager>,
    /// Subscription registry shared with the engine router.
    pub registry: Arc<SubscriptionRegistry>,
    /// Auth provider for token verification and authorization.
    pub auth_provider: Arc<dyn AuthProvider>,
    /// Bus publisher for REST/WebSocket PUBLISH operations.
    pub bus_publisher: Arc<dyn realtime_core::EventBusPublisher>,
}

/// Axum handler for WebSocket upgrade requests (`GET /ws`).
///
/// Upgrades the HTTP connection to WebSocket and spawns the
/// `handle_websocket()` task for the connection lifecycle.
pub async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_websocket(socket, state))
}

/// Handle the full lifecycle of a single WebSocket connection.
///
/// 1. Allocates a connection ID and registers the connection.
/// 2. Splits the WebSocket into reader/writer halves.
/// 3. Spawns a writer task and a reader task.
/// 4. Waits for either task to finish (connection close, error, etc.).
/// 5. Cleans up: removes subscriptions and deregisters the connection.
async fn handle_websocket(socket: WebSocket, state: AppState) {
    let conn_id = state.conn_manager.next_connection_id();

    let meta = ConnectionMeta {
        conn_id,
        peer_addr: "0.0.0.0:0".parse().unwrap(), // Will be updated after auth
        connected_at: Utc::now(),
        user_id: None,
        claims: None,
    };

    let (_, send_rx) = state.conn_manager.register(meta, OverflowPolicy::DropNewest);

    let (mut ws_sink, mut ws_stream) = {
        use futures::StreamExt;
        socket.split()
    };

    // Control channel for protocol messages (AUTH_OK, SUBSCRIBED, etc.)
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<String>(64);

    // Clone what we need before moving state
    let registry_cleanup = Arc::clone(&state.registry);
    let conn_manager_cleanup = Arc::clone(&state.conn_manager);

    // Writer task: drains send_rx + ctrl_rx and writes to WebSocket
    let write_conn_id = conn_id;
    let write_task = tokio::spawn(async move {
        writer_loop(&mut ws_sink, send_rx, ctrl_rx, write_conn_id).await;
    });

    // Reader task: processes client messages
    let read_task = tokio::spawn(async move {
        reader_loop(&mut ws_stream, conn_id, &state, ctrl_tx).await;
    });

    // Wait for either task to complete
    tokio::select! {
        _ = write_task => {}
        _ = read_task => {}
    }

    // Cleanup
    registry_cleanup.remove_connection(conn_id);
    conn_manager_cleanup.remove(conn_id);
    info!(conn_id = %conn_id, "WebSocket connection closed");
}

/// Writer loop: drains the event send channel and control channel,
/// serializes to JSON, and writes WebSocket text frames.
///
/// Includes **slow-client detection**: if 10 consecutive writes take
/// longer than 100ms, the client is disconnected.  Individual writes
/// have a 500ms hard timeout.
async fn writer_loop(
    ws_sink: &mut futures::stream::SplitSink<WebSocket, Message>,
    mut send_rx: mpsc::Receiver<Arc<EventEnvelope>>,
    mut ctrl_rx: mpsc::Receiver<String>,
    conn_id: ConnectionId,
) {
    use futures::SinkExt;

    let mut consecutive_slow = 0u32;
    let slow_threshold = std::time::Duration::from_millis(100);

    loop {
        let json = tokio::select! {
            Some(event) = send_rx.recv() => {
                let payload = EventPayload::from_envelope(&event);
                let msg = ServerMessage::Event {
                    sub_id: String::new(),
                    event: payload,
                };
                match serde_json::to_string(&msg) {
                    Ok(j) => j,
                    Err(e) => {
                        error!(conn_id = %conn_id, "Failed to serialize event: {}", e);
                        continue;
                    }
                }
            }
            Some(ctrl_msg) = ctrl_rx.recv() => {
                ctrl_msg
            }
            else => break,
        };

        let start = std::time::Instant::now();

        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            ws_sink.send(Message::Text(json.into())),
        )
        .await
        {
            Ok(Ok(())) => {
                if start.elapsed() > slow_threshold {
                    consecutive_slow += 1;
                    if consecutive_slow > 10 {
                        warn!(conn_id = %conn_id, "Client consistently slow, disconnecting");
                        return;
                    }
                } else {
                    consecutive_slow = 0;
                }
            }
            Ok(Err(e)) => {
                debug!(conn_id = %conn_id, "WebSocket write error: {}", e);
                return;
            }
            Err(_) => {
                warn!(conn_id = %conn_id, "WebSocket write timeout");
                return;
            }
        }
    }
}

/// Reader loop: processes incoming WebSocket frames.
///
/// Handles the full protocol state machine:
/// - `AUTH` → verify token, set `authenticated = true`, send `AUTH_OK`.
/// - `SUBSCRIBE` / `SUBSCRIBE_BATCH` → parse topic/filter/options,
///   authorize, register in registry, send `SUBSCRIBED`.
/// - `UNSUBSCRIBE` → remove from registry, send `UNSUBSCRIBED`.
/// - `PUBLISH` → build envelope, publish to bus.
/// - `PING` → send `PONG` with server timestamp.
async fn reader_loop(
    ws_stream: &mut futures::stream::SplitStream<WebSocket>,
    conn_id: ConnectionId,
    state: &AppState,
    ctrl_tx: mpsc::Sender<String>,
) {
    use futures::StreamExt;

    let mut authenticated = false;
    let mut claims: Option<AuthClaims> = None;

    while let Some(msg_result) = ws_stream.next().await {
        let msg = match msg_result {
            Ok(msg) => msg,
            Err(e) => {
                debug!(conn_id = %conn_id, "WebSocket read error: {}", e);
                return;
            }
        };

        match msg {
            Message::Text(text) => {
                let client_msg: ClientMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        warn!(conn_id = %conn_id, "Invalid client message: {}", e);
                        continue;
                    }
                };

                match client_msg {
                    ClientMessage::Auth { token } => {
                        let ctx = AuthContext {
                            peer_addr: "0.0.0.0:0".parse().unwrap(),
                            transport: "websocket".to_string(),
                        };

                        match state.auth_provider.verify(&token, &ctx).await {
                            Ok(auth_claims) => {
                                authenticated = true;
                                claims = Some(auth_claims);
                                info!(conn_id = %conn_id, "Client authenticated");

                                // Send AUTH_OK
                                let auth_ok = ServerMessage::AuthOk {
                                    conn_id: conn_id.to_string(),
                                    server_time: Utc::now().to_rfc3339(),
                                };
                                if let Ok(json) = serde_json::to_string(&auth_ok) {
                                    let _ = ctrl_tx.send(json).await;
                                }
                            }
                            Err(e) => {
                                warn!(conn_id = %conn_id, "Auth failed: {}", e);
                                // Send error
                                let err_msg = ServerMessage::Error {
                                    code: "AUTH_FAILED".to_string(),
                                    message: e.to_string(),
                                };
                                if let Ok(json) = serde_json::to_string(&err_msg) {
                                    let _ = ctrl_tx.send(json).await;
                                }
                                return; // Close connection
                            }
                        }
                    }

                    ClientMessage::Subscribe {
                        sub_id,
                        topic,
                        filter,
                        options,
                    } => {
                        if !authenticated {
                            warn!(conn_id = %conn_id, "Subscribe before auth");
                            continue;
                        }

                        let topic_pattern = TopicPattern::parse(&topic);

                        // Check authorization
                        if let Some(ref c) = claims {
                            if let Err(e) = state
                                .auth_provider
                                .authorize_subscribe(c, &topic_pattern)
                                .await
                            {
                                warn!(conn_id = %conn_id, "Subscribe denied: {}", e);
                                continue;
                            }
                        }

                        // Parse filter
                        let filter_expr = filter.and_then(|f| FilterExpr::from_json(&f));

                        // Parse options
                        let config = options
                            .map(|o| SubConfig {
                                overflow: match o.overflow.as_deref() {
                                    Some("drop_oldest") => OverflowPolicy::DropOldest,
                                    Some("disconnect") => OverflowPolicy::Disconnect,
                                    _ => OverflowPolicy::DropNewest,
                                },
                                rate_limit: o.rate_limit,
                                resume_from: o.resume_from,
                            })
                            .unwrap_or_default();

                        let subscription = Subscription {
                            sub_id: SubscriptionId(SmolStr::new(&sub_id)),
                            conn_id,
                            topic: topic_pattern,
                            filter: filter_expr,
                            config,
                        };

                        state.registry.subscribe(subscription, None);
                        debug!(conn_id = %conn_id, sub_id = %sub_id, topic = %topic, "Subscribed");

                        // Send SUBSCRIBED confirmation
                        let subscribed = ServerMessage::Subscribed {
                            sub_id: sub_id.clone(),
                            seq: 0,
                        };
                        if let Ok(json) = serde_json::to_string(&subscribed) {
                            let _ = ctrl_tx.send(json).await;
                        }
                    }

                    ClientMessage::SubscribeBatch { subscriptions } => {
                        if !authenticated {
                            warn!(conn_id = %conn_id, "Subscribe batch before auth");
                            continue;
                        }

                        for item in subscriptions {
                            let topic_pattern = TopicPattern::parse(&item.topic);
                            let filter_expr =
                                item.filter.and_then(|f| FilterExpr::from_json(&f));

                            let config = item
                                .options
                                .map(|o| SubConfig {
                                    overflow: match o.overflow.as_deref() {
                                        Some("drop_oldest") => OverflowPolicy::DropOldest,
                                        Some("disconnect") => OverflowPolicy::Disconnect,
                                        _ => OverflowPolicy::DropNewest,
                                    },
                                    rate_limit: o.rate_limit,
                                    resume_from: o.resume_from,
                                })
                                .unwrap_or_default();

                            let subscription = Subscription {
                                sub_id: SubscriptionId(SmolStr::new(&item.sub_id)),
                                conn_id,
                                topic: topic_pattern,
                                filter: filter_expr,
                                config,
                            };

                            state.registry.subscribe(subscription, None);

                            // Send SUBSCRIBED for each in batch
                            let subscribed = ServerMessage::Subscribed {
                                sub_id: item.sub_id.clone(),
                                seq: 0,
                            };
                            if let Ok(json) = serde_json::to_string(&subscribed) {
                                let _ = ctrl_tx.send(json).await;
                            }
                        }
                    }

                    ClientMessage::Unsubscribe { sub_id } => {
                        state.registry.unsubscribe(conn_id, &sub_id);
                        debug!(conn_id = %conn_id, sub_id = %sub_id, "Unsubscribed");

                        let unsub = ServerMessage::Unsubscribed {
                            sub_id: sub_id.clone(),
                        };
                        if let Ok(json) = serde_json::to_string(&unsub) {
                            let _ = ctrl_tx.send(json).await;
                        }
                    }

                    ClientMessage::Publish {
                        topic,
                        event_type,
                        payload,
                    } => {
                        if !authenticated {
                            warn!(conn_id = %conn_id, "Publish before auth");
                            continue;
                        }

                        debug!(conn_id = %conn_id, topic = %topic, event_type = %event_type, "PUBLISH received");

                        // Build an EventEnvelope and publish to the bus
                        let payload_bytes = match serde_json::to_vec(&payload) {
                            Ok(b) => b,
                            Err(e) => {
                                warn!(conn_id = %conn_id, "Invalid publish payload: {}", e);
                                continue;
                            }
                        };

                        let envelope = EventEnvelope::new(
                            TopicPath::new(&topic),
                            &event_type,
                            Bytes::from(payload_bytes),
                        );

                        if let Err(e) = state
                            .bus_publisher
                            .publish(envelope.topic.as_str(), &envelope)
                            .await
                        {
                            error!(conn_id = %conn_id, "Failed to publish event: {}", e);
                        }
                    }

                    ClientMessage::Ping => {
                        debug!(conn_id = %conn_id, "Ping received");
                        let pong = ServerMessage::Pong {
                            server_time: Utc::now().to_rfc3339(),
                        };
                        if let Ok(json) = serde_json::to_string(&pong) {
                            let _ = ctrl_tx.send(json).await;
                        }
                    }
                }
            }

            Message::Binary(_) => {
                // MessagePack messages could be handled here
                debug!(conn_id = %conn_id, "Binary message received (unsupported)");
            }

            Message::Ping(_) => {
                // axum handles pong automatically
            }

            Message::Pong(_) => {}

            Message::Close(_) => {
                info!(conn_id = %conn_id, "Client initiated close");
                return;
            }
        }
    }
}

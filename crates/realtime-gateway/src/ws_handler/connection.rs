/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   connection.rs                                      :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/05/18 21:19:15 by dlesieur          #+#    #+#             */
/*   Updated: 2026/05/18 21:19:15 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::WebSocket;
use chrono::Utc;
use futures::StreamExt;
use realtime_core::{ConnectionId, ConnectionMeta, OverflowPolicy};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

/// How long `handle_websocket` waits for the writer to put the answering
/// Close frame on the wire. Bounded: cleanup must not depend on a peer.
const GOODBYE_GRACE: std::time::Duration = std::time::Duration::from_millis(700);

use super::reader::{reader_loop, Ending};
use super::writer::{send_close, writer_loop};
use super::AppState;

fn default_peer_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 0))
}

fn create_connection_meta(conn_id: ConnectionId) -> ConnectionMeta {
    ConnectionMeta {
        conn_id,
        peer_addr: default_peer_addr(),
        connected_at: Utc::now(),
        user_id: None,
        claims: None,
    }
}

pub async fn handle_websocket(socket: WebSocket, state: AppState) {
    let conn_id = state.conn_manager.next_connection_id();
    let meta = create_connection_meta(conn_id);
    let (_, send_rx) = state
        .conn_manager
        .register(meta, OverflowPolicy::DropNewest);
    let (ws_sink, ws_stream) = socket.split();
    let (ctrl_tx, ctrl_rx) = mpsc::channel::<String>(64);
    let registry = Arc::clone(&state.registry);
    let conn_manager = Arc::clone(&state.conn_manager);
    // RFC 6455 §5.5.1: a Close frame must be answered with a Close frame. The
    // sink lives in the writer task, so the reader cannot answer it itself --
    // this channel is how the answer is asked for. Before it existed the
    // socket was just dropped and every client that closed politely was told
    // 1006 (abnormal closure), which is indistinguishable from the network
    // dying: SDKs reconnect with backoff and log an error for what was a
    // normal goodbye.
    let (goodbye_tx, goodbye_rx) = oneshot::channel::<()>();
    let mut writer = tokio::spawn(writer_loop(ws_sink, send_rx, ctrl_rx, goodbye_rx, conn_id));
    let mut reader = tokio::spawn(reader_loop(ws_stream, conn_id, state, ctrl_tx));
    tokio::select! {
        handed_back = &mut writer => {
            // The write side ended first. From here that is what a peer
            // closing mid-frame looks like: the pending send fails. The read
            // side may still be about to report that peer's Close frame, and
            // it is owed an answer -- the writer hands the sink back so there
            // is still something to answer with.
            let mut sink = handed_back.ok().flatten();
            if matches!(
                tokio::time::timeout(GOODBYE_GRACE, &mut reader).await,
                Ok(Ok(Ending::ClientClose))
            ) {
                if let Some(sink) = sink.as_mut() {
                    send_close(sink, conn_id).await;
                } else {
                    // the write side is gone with the sink: nothing left to
                    // answer with, and this peer will see 1006.
                    warn!(conn_id = %conn_id, "client close went unanswered: no sink");
                }
            }
        }
        ending = &mut reader => {
            if matches!(ending, Ok(Ending::ClientClose)) {
                // The writer sends the Close frame and returns; wait for it so
                // the frame is on the wire before the socket is dropped below.
                let _ = goodbye_tx.send(());
                if tokio::time::timeout(GOODBYE_GRACE, &mut writer).await.is_err() {
                    warn!(conn_id = %conn_id, "client close went unanswered: the writer did not finish in {GOODBYE_GRACE:?}");
                }
            }
        }
    }
    registry.remove_connection(conn_id);
    conn_manager.remove(conn_id);
    info!(conn_id = %conn_id, "WebSocket connection closed");
}

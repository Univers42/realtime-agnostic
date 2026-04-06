/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   rest_api.rs                                        :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:32 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:14:21 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! REST API handlers for event publishing and health checks.
//!
//! Provides HTTP endpoints alongside the WebSocket gateway:
//!
//! | Method | Path                | Description                    |
//! |--------|---------------------|--------------------------------|
//! | `POST` | `/v1/publish`       | Publish a single event         |
//! | `POST` | `/v1/publish/batch` | Publish up to 1000 events      |
//! | `GET`  | `/v1/health`        | Health check + connection stats|
//!
//! All endpoints accept and return JSON. Payload size is limited to
//! 64 KB per event.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use bytes::Bytes;
use realtime_core::{
    BatchPublishRequest, BatchPublishResponse,
    EventEnvelope, HealthResponse, PublishRequest, PublishResponse, TopicPath,
};
use tracing::{debug, error};

use crate::ws_handler::AppState;

/// `POST /v1/publish` — publish a single event.
///
/// Validates the topic (non-empty) and payload size (≤64 KB), builds
/// an [`EventEnvelope`], publishes it to the event bus, and returns
/// a [`PublishResponse`] with the assigned event ID and sequence number.
pub async fn publish_event(
    State(state): State<AppState>,
    Json(req): Json<PublishRequest>,
) -> impl IntoResponse {
    // Validate topic
    if req.topic.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Topic is required" })),
        );
    }

    // Validate payload size
    let payload_bytes = match serde_json::to_vec(&req.payload) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Invalid payload: {}", e) })),
            );
        }
    };

    if payload_bytes.len() > 65_536 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({ "error": "Payload exceeds 64KB limit" })),
        );
    }

    // Build event envelope
    let event = EventEnvelope::new(
        TopicPath::new(&req.topic),
        &req.event_type,
        Bytes::from(payload_bytes),
    );

    // Publish to bus
    match state.bus_publisher.publish(&req.topic, &event).await {
        Ok(receipt) => {
            debug!(event_id = %receipt.event_id, topic = %req.topic, "Event published");
            (
                StatusCode::OK,
                Json(serde_json::json!(PublishResponse {
                    event_id: receipt.event_id.to_string(),
                    sequence: receipt.sequence,
                    delivered_to_bus: receipt.delivered_to_bus,
                })),
            )
        }
        Err(e) => {
            error!("Failed to publish event: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Publish failed: {}", e) })),
            )
        }
    }
}

/// `POST /v1/publish/batch` — publish up to 1000 events atomically.
///
/// Validates all payloads before publishing. If any single payload
/// exceeds 64 KB, the entire batch is rejected.
pub async fn publish_batch(
    State(state): State<AppState>,
    Json(req): Json<BatchPublishRequest>,
) -> impl IntoResponse {
    if req.events.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "At least one event is required" })),
        );
    }

    if req.events.len() > 1000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Maximum 1000 events per batch" })),
        );
    }

    let mut events: Vec<(String, EventEnvelope)> = Vec::with_capacity(req.events.len());
    for item in &req.events {
        let payload_bytes = match serde_json::to_vec(&item.payload) {
            Ok(b) => b,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Invalid payload: {}", e) })),
                );
            }
        };

        if payload_bytes.len() > 65_536 {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(serde_json::json!({ "error": "One or more payloads exceed 64KB limit" })),
            );
        }

        let event = EventEnvelope::new(
            TopicPath::new(&item.topic),
            &item.event_type,
            Bytes::from(payload_bytes),
        );
        events.push((item.topic.clone(), event));
    }

    match state.bus_publisher.publish_batch(&events).await {
        Ok(receipts) => {
            let results: Vec<PublishResponse> = receipts
                .iter()
                .map(|r| PublishResponse {
                    event_id: r.event_id.to_string(),
                    sequence: r.sequence,
                    delivered_to_bus: r.delivered_to_bus,
                })
                .collect();

            (
                StatusCode::OK,
                Json(serde_json::json!(BatchPublishResponse { results })),
            )
        }
        Err(e) => {
            error!("Failed to publish batch: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Batch publish failed: {}", e) })),
            )
        }
    }
}

/// `GET /v1/health` — health check returning connection and subscription counts.
pub async fn health_check(
    State(state): State<AppState>,
) -> impl IntoResponse {
    let resp = HealthResponse {
        status: "ok".to_string(),
        connections: state.conn_manager.connection_count() as u64,
        subscriptions: state.registry.subscription_count() as u64,
        uptime_seconds: 0, // Would normally track server start time
    };

    (StatusCode::OK, Json(resp))
}

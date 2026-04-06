/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   producer.rs                                        :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:12:44 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:17:52 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! MongoDB CDC (Change Data Capture) producer using change streams.
//!
//! This module opens a MongoDB change stream on a database and converts
//! each change event into an [`EventEnvelope`] for the event bus.
//!
//! ## Requirements
//!
//! MongoDB change streams require a **replica set** or **sharded cluster**
//! (they need the oplog). A standalone `mongod` will not work.
//!
//! ## How it works
//!
//! 1. Connects to MongoDB using the official `mongodb` driver.
//! 2. Opens a database-level change stream with optional collection filter.
//! 3. Each change event is parsed by [`parse_change_event()`] into an envelope.
//! 4. Events are forwarded through an `mpsc` channel to the bus publisher.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use mongodb::{
    bson::{doc, Document},
    options::{ChangeStreamOptions, FullDocumentType},
    Client,
};
use realtime_core::{
    DatabaseProducer, EventEnvelope, EventSource, EventStream, RealtimeError, Result,
    SourceKind, TopicPath,
};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::config::MongoConfig;

/// MongoDB change stream producer.
///
/// Watches MongoDB change streams and converts change events into
/// [`EventEnvelope`]s for the realtime event bus.
///
/// Requires a MongoDB replica set or sharded cluster (change streams
/// need the oplog).
pub struct MongoProducer {
    config: MongoConfig,
    running: Arc<AtomicBool>,
}

impl MongoProducer {
    /// Create a new MongoDB CDC producer from config.
    ///
    /// Does **not** connect yet — call [`start()`](DatabaseProducer::start)
    /// to open the change stream.
    pub fn new(config: MongoConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
        }
    }
}

#[async_trait]
impl DatabaseProducer for MongoProducer {
    async fn start(&self) -> Result<Box<dyn EventStream>> {
        self.running.store(true, Ordering::SeqCst);

        let client = Client::with_uri_str(&self.config.uri)
            .await
            .map_err(|e| RealtimeError::Internal(format!("MongoDB connect failed: {}", e)))?;

        let db = client.database(&self.config.database);
        let (tx, rx) = mpsc::channel::<EventEnvelope>(4096);

        let running = Arc::clone(&self.running);
        let config = self.config.clone();

        tokio::spawn(async move {
            // Build pipeline filter for specific operations if configured
            let pipeline = if !config.collections.is_empty() {
                let collection_names: Vec<&str> =
                    config.collections.iter().map(|c| c.name.as_str()).collect();
                vec![doc! {
                    "$match": {
                        "ns.coll": { "$in": collection_names }
                    }
                }]
            } else {
                vec![]
            };

            let full_doc_type = match config.full_document.as_str() {
                "updateLookup" => Some(FullDocumentType::UpdateLookup),
                "whenAvailable" => Some(FullDocumentType::WhenAvailable),
                "required" => Some(FullDocumentType::Required),
                _ => None,
            };

            let mut options = ChangeStreamOptions::default();
            options.full_document = full_doc_type;

            let change_stream_result = db.watch().pipeline(pipeline).await;

            let mut change_stream = match change_stream_result {
                Ok(cs) => cs,
                Err(e) => {
                    error!("Failed to open change stream: {}", e);
                    return;
                }
            };

            info!(
                database = %config.database,
                "MongoDB change stream producer started"
            );

            while running.load(Ordering::SeqCst) {
                match change_stream.next().await {
                    Some(Ok(change_event)) => {
                        // Serialize the ChangeStreamEvent to a BSON Document for parsing
                        if let Ok(raw_doc) = mongodb::bson::to_document(&change_event) {
                            if let Some(event) =
                                parse_change_event(&raw_doc, &config.topic_prefix, &config.database)
                            {
                                if tx.send(event).await.is_err() {
                                    warn!("Event channel closed, stopping MongoDB producer");
                                    break;
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        error!("Change stream error: {}", e);
                        // Could implement reconnect logic here
                        break;
                    }
                    None => {
                        info!("Change stream ended");
                        break;
                    }
                }
            }

            info!("MongoDB change stream producer stopped");
        });

        Ok(Box::new(MongoEventStream { rx }))
    }

    async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        info!("MongoDB CDC producer stopped");
        Ok(())
    }

    async fn health_check(&self) -> Result<()> {
        let client = Client::with_uri_str(&self.config.uri)
            .await
            .map_err(|e| {
                RealtimeError::Internal(format!("MongoDB health check connect failed: {}", e))
            })?;

        client
            .database(&self.config.database)
            .run_command(doc! { "ping": 1 })
            .await
            .map_err(|e| {
                RealtimeError::Internal(format!("MongoDB health check ping failed: {}", e))
            })?;

        Ok(())
    }

    fn name(&self) -> &str {
        "mongodb"
    }
}

/// Parse a MongoDB change stream BSON document into an [`EventEnvelope`].
///
/// Maps MongoDB operation types to event types:
/// `insert → inserted`, `update → updated`, `replace → replaced`, `delete → deleted`.
///
/// The resulting topic is `"{topic_prefix}/{collection}/{event_type}"`.
///
/// # Arguments
///
/// * `doc` — The raw BSON change event document.
/// * `topic_prefix` — Prefix for the topic (e.g. `"mongo"`).
/// * `database` — The database name (included in source metadata).
///
/// # Returns
///
/// `Some(EventEnvelope)` on success, `None` if essential fields are missing.
pub fn parse_change_event(
    doc: &Document,
    topic_prefix: &str,
    database: &str,
) -> Option<EventEnvelope> {
    let operation_type = doc.get_str("operationType").ok()?;
    let ns = doc.get_document("ns").ok()?;
    let collection = ns.get_str("coll").ok()?;

    let event_type = match operation_type {
        "insert" => "inserted",
        "update" => "updated",
        "replace" => "replaced",
        "delete" => "deleted",
        other => other,
    };

    let topic = format!("{}/{}/{}", topic_prefix, collection, event_type);

    // Build payload from the change event data
    let payload_doc = doc! {
        "operation": operation_type,
        "collection": collection,
        "database": database,
        "fullDocument": doc.get("fullDocument").cloned().unwrap_or(mongodb::bson::Bson::Null),
        "documentKey": doc.get("documentKey").cloned().unwrap_or(mongodb::bson::Bson::Null),
    };

    // Convert BSON to JSON bytes
    let json_value = bson_to_json(&mongodb::bson::Bson::Document(payload_doc));
    let payload_bytes = serde_json::to_vec(&json_value).ok()?;

    let mut metadata = HashMap::new();
    metadata.insert("database".to_string(), database.to_string());
    metadata.insert("collection".to_string(), collection.to_string());

    let mut envelope = EventEnvelope::new(
        TopicPath::new(&topic),
        event_type,
        Bytes::from(payload_bytes),
    );

    envelope.source = Some(EventSource {
        kind: SourceKind::Database,
        id: format!("mongodb:{}.{}", database, collection),
        metadata,
    });

    Some(envelope)
}

/// Convert a BSON value to a `serde_json::Value`.
///
/// Handles all BSON types: strings, numbers, booleans, arrays, documents,
/// ObjectIds (converted to hex string), DateTime (to string), etc.
fn bson_to_json(bson: &mongodb::bson::Bson) -> serde_json::Value {
    match bson {
        mongodb::bson::Bson::Null => serde_json::Value::Null,
        mongodb::bson::Bson::Boolean(b) => serde_json::Value::Bool(*b),
        mongodb::bson::Bson::Int32(i) => serde_json::Value::Number((*i).into()),
        mongodb::bson::Bson::Int64(i) => serde_json::Value::Number((*i).into()),
        mongodb::bson::Bson::Double(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        mongodb::bson::Bson::String(s) => serde_json::Value::String(s.clone()),
        mongodb::bson::Bson::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(bson_to_json).collect())
        }
        mongodb::bson::Bson::Document(doc) => {
            let map: serde_json::Map<String, serde_json::Value> = doc
                .iter()
                .map(|(k, v)| (k.clone(), bson_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        mongodb::bson::Bson::ObjectId(oid) => serde_json::Value::String(oid.to_hex()),
        mongodb::bson::Bson::DateTime(dt) => {
            serde_json::Value::String(dt.to_string())
        }
        _ => serde_json::Value::String(bson.to_string()),
    }
}

/// Internal event stream backed by an `mpsc::Receiver`.
struct MongoEventStream {
    rx: mpsc::Receiver<EventEnvelope>,
}

#[async_trait]
impl EventStream for MongoEventStream {
    async fn next_event(&mut self) -> Option<EventEnvelope> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn test_parse_insert_change_event() {
        let change_doc = doc! {
            "operationType": "insert",
            "ns": {
                "db": "testdb",
                "coll": "orders"
            },
            "fullDocument": {
                "_id": "abc123",
                "status": "pending",
                "total": 99.99
            },
            "documentKey": {
                "_id": "abc123"
            }
        };

        let event = parse_change_event(&change_doc, "mongo", "testdb").unwrap();
        assert_eq!(event.topic.as_str(), "mongo/orders/inserted");
        assert_eq!(event.event_type, "inserted");

        let source = event.source.unwrap();
        assert!(matches!(source.kind, SourceKind::Database));
        assert_eq!(source.id, "mongodb:testdb.orders");
    }

    #[test]
    fn test_parse_update_change_event() {
        let change_doc = doc! {
            "operationType": "update",
            "ns": {
                "db": "testdb",
                "coll": "orders"
            },
            "fullDocument": {
                "_id": "abc123",
                "status": "completed",
                "total": 99.99
            },
            "documentKey": {
                "_id": "abc123"
            },
            "updateDescription": {
                "updatedFields": {
                    "status": "completed"
                },
                "removedFields": []
            }
        };

        let event = parse_change_event(&change_doc, "mongo", "testdb").unwrap();
        assert_eq!(event.topic.as_str(), "mongo/orders/updated");
        assert_eq!(event.event_type, "updated");
    }

    #[test]
    fn test_parse_delete_change_event() {
        let change_doc = doc! {
            "operationType": "delete",
            "ns": {
                "db": "testdb",
                "coll": "users"
            },
            "documentKey": {
                "_id": "xyz789"
            }
        };

        let event = parse_change_event(&change_doc, "db", "testdb").unwrap();
        assert_eq!(event.topic.as_str(), "db/users/deleted");
        assert_eq!(event.event_type, "deleted");
    }

    #[test]
    fn test_bson_to_json() {
        let bson = mongodb::bson::Bson::Document(doc! {
            "name": "test",
            "value": 42,
            "nested": {
                "key": "val"
            }
        });

        let json = bson_to_json(&bson);
        assert_eq!(json["name"], "test");
        assert_eq!(json["value"], 42);
        assert_eq!(json["nested"]["key"], "val");
    }
}

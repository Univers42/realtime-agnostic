/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   producer.rs                                        :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:12:55 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:15:43 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! PostgreSQL CDC (Change Data Capture) producer using `LISTEN/NOTIFY`.
//!
//! This module listens to a PostgreSQL notification channel and converts
//! each notification payload (JSON) into an [`EventEnvelope`] that is
//! published to the event bus.
//!
//! ## How it works
//!
//! 1. Connects to PostgreSQL using `tokio-postgres`.
//! 2. Issues `LISTEN {channel}` (default: `realtime_events`).
//! 3. Drives the connection future in a background task.
//! 4. When a trigger fires, `pg_notify()` sends a JSON payload.
//! 5. The producer parses it into an `EventEnvelope` and forwards it.
//!
//! ## Required PostgreSQL setup
//!
//! A trigger function must exist that calls `pg_notify()` on DML events.
//! Use [`PostgresProducer::generate_trigger_sql()`] to generate the SQL.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use bytes::Bytes;

use futures::StreamExt;
use realtime_core::{
    DatabaseProducer, EventEnvelope, EventSource, EventStream, RealtimeError, Result,
    SourceKind, TopicPath,
};
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, NoTls};
use tracing::{debug, error, info, warn};

use crate::config::PostgresConfig;

/// PostgreSQL CDC producer using LISTEN/NOTIFY.
///
/// This watches for PostgreSQL notifications on a configured channel
/// and converts them into EventEnvelopes published to the event bus.
///
/// #### Setup required on PostgreSQL:
///
/// ```sql
/// -- Create the notification function
/// CREATE OR REPLACE FUNCTION realtime_notify() RETURNS trigger AS $$
/// DECLARE
///   payload json;
/// BEGIN
///   payload = json_build_object(
///     'table', TG_TABLE_NAME,
///     'schema', TG_TABLE_SCHEMA,
///     'operation', TG_OP,
///     'data', CASE
///       WHEN TG_OP = 'DELETE' THEN row_to_json(OLD)
///       ELSE row_to_json(NEW)
///     END,
///     'old_data', CASE
///       WHEN TG_OP = 'UPDATE' THEN row_to_json(OLD)
///       ELSE NULL
///     END
///   );
///   PERFORM pg_notify('realtime_events', payload::text);
///   RETURN NEW;
/// END;
/// $$ LANGUAGE plpgsql;
///
/// -- Attach to any table you want to watch:
/// CREATE TRIGGER orders_realtime
///   AFTER INSERT OR UPDATE OR DELETE ON orders
///   FOR EACH ROW EXECUTE FUNCTION realtime_notify();
/// ```
pub struct PostgresProducer {
    config: PostgresConfig,
    running: Arc<AtomicBool>,
    /// Holds the PG client after LISTEN to keep the TCP connection alive.
    client: std::sync::Mutex<Option<tokio_postgres::Client>>,
}

impl PostgresProducer {
    /// Create a new PostgreSQL CDC producer from config.
    ///
    /// Does **not** connect to the database yet — call
    /// [`start()`](DatabaseProducer::start) to begin listening.
    pub fn new(config: PostgresConfig) -> Self {
        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            client: std::sync::Mutex::new(None),
        }
    }

    /// Generate the SQL DDL for the notification trigger function.
    ///
    /// Returns a complete SQL script that:
    /// 1. Creates `realtime_notify()` trigger function.
    /// 2. Drops any existing trigger on the table.
    /// 3. Creates a new `AFTER INSERT OR UPDATE OR DELETE` trigger.
    ///
    /// # Arguments
    ///
    /// * `table` — The table name (e.g. `"orders"`).
    /// * `channel` — The NOTIFY channel name (e.g. `"realtime_events"`).
    pub fn generate_trigger_sql(table: &str, channel: &str) -> String {
        format!(
            r#"
-- Notification function
CREATE OR REPLACE FUNCTION realtime_notify() RETURNS trigger AS $$
DECLARE
  payload json;
BEGIN
  payload = json_build_object(
    'table', TG_TABLE_NAME,
    'schema', TG_TABLE_SCHEMA,
    'operation', TG_OP,
    'data', CASE
      WHEN TG_OP = 'DELETE' THEN row_to_json(OLD)
      ELSE row_to_json(NEW)
    END,
    'old_data', CASE
      WHEN TG_OP = 'UPDATE' THEN row_to_json(OLD)
      ELSE NULL
    END
  );
  PERFORM pg_notify('{channel}', payload::text);
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Trigger on {table}
DROP TRIGGER IF EXISTS {table}_realtime ON {table};
CREATE TRIGGER {table}_realtime
  AFTER INSERT OR UPDATE OR DELETE ON {table}
  FOR EACH ROW EXECUTE FUNCTION realtime_notify();
"#,
            channel = channel,
            table = table,
        )
    }
}

#[async_trait]
impl DatabaseProducer for PostgresProducer {
    async fn start(&self) -> Result<Box<dyn EventStream>> {
        self.running.store(true, Ordering::SeqCst);

        let (client, connection) =
            tokio_postgres::connect(&self.config.connection_string, NoTls)
                .await
                .map_err(|e| RealtimeError::Internal(format!("PostgreSQL connect failed: {}", e)))?;

        let (tx, rx) = mpsc::channel::<EventEnvelope>(4096);
        let channel = self.config.channel.clone();
        let topic_prefix = self.config.topic_prefix.clone();
        let running = Arc::clone(&self.running);

        // Spawn the connection driver FIRST.
        // tokio-postgres requires the Connection future to be driven
        // concurrently for the Client to work. poll_message() drives
        // the TCP protocol and yields notifications.
        let mut connection = connection;
        tokio::spawn(async move {
            let mut stream = futures::stream::poll_fn(move |cx| {
                connection.poll_message(cx)
            });

            loop {
                if !running.load(Ordering::SeqCst) {
                    break;
                }

                match stream.next().await {
                    Some(Ok(AsyncMessage::Notification(notification))) => {
                        debug!(
                            channel = %notification.channel(),
                            "Received PostgreSQL notification"
                        );
                        if let Some(event) = parse_pg_notification(
                            notification.payload(),
                            &topic_prefix,
                        ) {
                            if tx.send(event).await.is_err() {
                                warn!("Event channel closed, stopping PostgreSQL producer");
                                break;
                            }
                        }
                    }
                    Some(Ok(AsyncMessage::Notice(notice))) => {
                        debug!("PostgreSQL notice: {}", notice.message());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        error!("PostgreSQL connection error: {}", e);
                        break;
                    }
                    None => {
                        info!("PostgreSQL connection closed");
                        break;
                    }
                }
            }
        });

        // Connection is now being driven — LISTEN will work.
        let listen_query = format!("LISTEN {}", channel);
        client
            .batch_execute(&listen_query)
            .await
            .map_err(|e| RealtimeError::Internal(format!("LISTEN failed: {}", e)))?;

        info!(channel = %channel, "PostgreSQL CDC producer started");

        // Store the client to keep the TCP connection alive.
        // Dropping it would close the sender half, which could
        // terminate the connection driver task.
        *self.client.lock().unwrap() = Some(client);

        Ok(Box::new(PostgresEventStream { rx }))
    }

    async fn stop(&self) -> Result<()> {
        self.running.store(false, Ordering::SeqCst);
        // Drop the client to close the TCP connection
        *self.client.lock().unwrap() = None;
        info!("PostgreSQL CDC producer stopped");
        Ok(())
    }

    async fn health_check(&self) -> Result<()> {
        let (client, connection) =
            tokio_postgres::connect(&self.config.connection_string, NoTls)
                .await
                .map_err(|e| {
                    RealtimeError::Internal(format!("PostgreSQL health check failed: {}", e))
                })?;

        tokio::spawn(async move {
            if let Err(e) = connection.await {
                error!("PostgreSQL health check connection error: {}", e);
            }
        });

        client
            .simple_query("SELECT 1")
            .await
            .map_err(|e| {
                RealtimeError::Internal(format!("PostgreSQL health check query failed: {}", e))
            })?;

        Ok(())
    }

    fn name(&self) -> &str {
        "postgresql"
    }
}

/// Parse a PostgreSQL `pg_notify()` JSON payload into an [`EventEnvelope`].
///
/// Expected JSON format:
/// ```json
/// {
///   "table": "orders",
///   "schema": "public",
///   "operation": "INSERT",
///   "data": { "id": 1, "status": "pending" },
///   "old_data": null
/// }
/// ```
///
/// The resulting event topic is `"{topic_prefix}/{table}/{event_type}"`
/// where `event_type` is mapped from the SQL operation:
/// `INSERT → inserted`, `UPDATE → updated`, `DELETE → deleted`.
///
/// # Returns
///
/// `Some(EventEnvelope)` on success, `None` if the JSON is invalid.
pub fn parse_pg_notification(
    payload: &str,
    topic_prefix: &str,
) -> Option<EventEnvelope> {
    let json: serde_json::Value = serde_json::from_str(payload).ok()?;

    let table = json.get("table")?.as_str()?;
    let schema = json.get("schema")?.as_str().unwrap_or("public");
    let operation = json.get("operation")?.as_str()?;
    // Ensure the data field exists in the payload
    let _data = json.get("data")?;

    let event_type = match operation {
        "INSERT" => "inserted",
        "UPDATE" => "updated",
        "DELETE" => "deleted",
        _ => operation,
    };

    let topic = format!("{}/{}/{}", topic_prefix, table, event_type);

    let mut metadata = HashMap::new();
    metadata.insert("schema".to_string(), schema.to_string());
    metadata.insert("table".to_string(), table.to_string());
    if let Some(old_data) = json.get("old_data") {
        if !old_data.is_null() {
            metadata.insert("has_old_data".to_string(), "true".to_string());
        }
    }

    let payload_bytes = serde_json::to_vec(&json).ok()?;

    let mut event = EventEnvelope::new(
        TopicPath::new(&topic),
        event_type,
        Bytes::from(payload_bytes),
    );

    event.source = Some(EventSource {
        kind: SourceKind::Database,
        id: format!("postgresql:{}.{}", schema, table),
        metadata,
    });

    Some(event)
}

/// Internal event stream backed by an `mpsc::Receiver`.
struct PostgresEventStream {
    rx: mpsc::Receiver<EventEnvelope>,
}

#[async_trait]
impl EventStream for PostgresEventStream {
    async fn next_event(&mut self) -> Option<EventEnvelope> {
        self.rx.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pg_notification_insert() {
        let payload = r#"{
            "table": "orders",
            "schema": "public",
            "operation": "INSERT",
            "data": {"id": 1, "status": "pending", "total": 99.99},
            "old_data": null
        }"#;

        let event = parse_pg_notification(payload, "db").unwrap();
        assert_eq!(event.topic.as_str(), "db/orders/inserted");
        assert_eq!(event.event_type, "inserted");
        assert!(event.source.is_some());

        let source = event.source.unwrap();
        assert!(matches!(source.kind, SourceKind::Database));
        assert_eq!(source.id, "postgresql:public.orders");
    }

    #[test]
    fn test_parse_pg_notification_update() {
        let payload = r#"{
            "table": "orders",
            "schema": "public",
            "operation": "UPDATE",
            "data": {"id": 1, "status": "completed", "total": 99.99},
            "old_data": {"id": 1, "status": "pending", "total": 99.99}
        }"#;

        let event = parse_pg_notification(payload, "db").unwrap();
        assert_eq!(event.topic.as_str(), "db/orders/updated");
        assert_eq!(event.event_type, "updated");
    }

    #[test]
    fn test_parse_pg_notification_delete() {
        let payload = r#"{
            "table": "users",
            "schema": "public",
            "operation": "DELETE",
            "data": {"id": 42, "name": "John"},
            "old_data": null
        }"#;

        let event = parse_pg_notification(payload, "mydb").unwrap();
        assert_eq!(event.topic.as_str(), "mydb/users/deleted");
        assert_eq!(event.event_type, "deleted");
    }

    #[test]
    fn test_parse_pg_notification_invalid_json() {
        let event = parse_pg_notification("not json", "db");
        assert!(event.is_none());
    }

    #[test]
    fn test_generate_trigger_sql() {
        let sql = PostgresProducer::generate_trigger_sql("orders", "realtime_events");
        assert!(sql.contains("CREATE TRIGGER orders_realtime"));
        assert!(sql.contains("pg_notify('realtime_events'"));
        assert!(sql.contains("AFTER INSERT OR UPDATE OR DELETE ON orders"));
    }
}

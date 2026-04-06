/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   main.rs                                            :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:44 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:19:25 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Binary entry point for the realtime server.
//!
//! Initialises tracing, loads [`ServerConfig`](realtime_server::config::ServerConfig)
//! from environment / config file, and calls [`run()`](realtime_server::server::run).
//!
//! ## Environment variables
//!
//! | Variable | Description |
//! |---|---|
//! | `REALTIME_CONFIG` | Path to a JSON config file |
//! | `REALTIME_HOST` | Bind address (default `0.0.0.0`) |
//! | `REALTIME_PORT` | Bind port (default `9090`) |
//! | `REALTIME_JWT_SECRET` | HMAC secret for JWT auth |
//! | `REALTIME_PG_URL` | PostgreSQL connection string |
//! | `REALTIME_MONGO_URI` | MongoDB connection URI |
//! | `RUST_LOG` | tracing filter (e.g. `info,realtime_engine=debug`) |

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing/logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

    // Load configuration
    let config = load_config()?;

    tracing::info!("Starting Realtime Engine v{}", env!("CARGO_PKG_VERSION"));
    tracing::info!("Event bus: {:?}", config.event_bus);
    tracing::info!("Auth: {:?}", config.auth);
    tracing::info!(
        "Performance: send_queue={}, fanout_workers={}, dispatch_capacity={}",
        config.performance.send_queue_capacity,
        config.performance.fanout_workers,
        config.performance.dispatch_channel_capacity,
    );

    realtime_server::server::run(config).await
}

/// Load server configuration from environment.
///
/// Priority:
/// 1. JSON file at `$REALTIME_CONFIG`
/// 2. Individual `REALTIME_*` env vars merged onto defaults
///
/// Database producers (PostgreSQL, MongoDB) are added when their
/// respective connection-string env vars are set.
fn load_config() -> anyhow::Result<realtime_server::config::ServerConfig> {
    // Check for config file path in env
    if let Ok(config_path) = std::env::var("REALTIME_CONFIG") {
        let content = std::fs::read_to_string(&config_path)?;
        let config: realtime_server::config::ServerConfig = serde_json::from_str(&content)?;
        return Ok(config);
    }

    // Check for individual env vars
    let mut config = realtime_server::config::ServerConfig::default();

    if let Ok(host) = std::env::var("REALTIME_HOST") {
        config.host = host;
    }
    if let Ok(port) = std::env::var("REALTIME_PORT") {
        config.port = port.parse()?;
    }
    if let Ok(secret) = std::env::var("REALTIME_JWT_SECRET") {
        config.auth = realtime_server::config::AuthConfig::Jwt {
            secret,
            issuer: std::env::var("REALTIME_JWT_ISSUER").ok(),
            audience: std::env::var("REALTIME_JWT_AUDIENCE").ok(),
        };
    }

    // ─── PostgreSQL CDC from env ────────────────────────────────────
    if let Ok(pg_url) = std::env::var("REALTIME_PG_URL") {
        let channel = std::env::var("REALTIME_PG_CHANNEL")
            .unwrap_or_else(|_| "realtime_events".to_string());
        let topic_prefix = std::env::var("REALTIME_PG_PREFIX")
            .unwrap_or_else(|_| "pg".to_string());

        config.databases.push(
            realtime_server::config::DatabaseConfig {
                adapter: "postgresql".to_string(),
                config: serde_json::json!({
                    "connection_string": pg_url,
                    "channel": channel,
                    "tables": [],
                    "topic_prefix": topic_prefix,
                    "poll_interval_ms": 100
                }),
            },
        );
        tracing::info!("PostgreSQL CDC configured from env");
    }

    // ─── MongoDB Change Streams from env ────────────────────────────
    if let Ok(mongo_uri) = std::env::var("REALTIME_MONGO_URI") {
        let database = std::env::var("REALTIME_MONGO_DB")
            .unwrap_or_else(|_| "syncspace".to_string());
        let topic_prefix = std::env::var("REALTIME_MONGO_PREFIX")
            .unwrap_or_else(|_| "mongo".to_string());

        config.databases.push(
            realtime_server::config::DatabaseConfig {
                adapter: "mongodb".to_string(),
                config: serde_json::json!({
                    "uri": mongo_uri,
                    "database": database,
                    "collections": [],
                    "topic_prefix": topic_prefix,
                    "full_document": "updateLookup"
                }),
            },
        );
        tracing::info!("MongoDB Change Streams configured from env");
    }

    Ok(config)
}

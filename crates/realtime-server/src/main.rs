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

use realtime_server::config::{AuthConfig, DatabaseConfig, ServerConfig};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(true)
        .init();

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

fn load_config() -> anyhow::Result<ServerConfig> {
    if let Ok(path) = std::env::var("REALTIME_CONFIG") {
        return load_config_from_file(&path);
    }
    let mut config = load_config_from_env()?;
    add_pg_config(&mut config);
    add_mongo_config(&mut config);
    Ok(config)
}

fn load_config_from_file(path: &str) -> anyhow::Result<ServerConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: ServerConfig = serde_json::from_str(&content)?;
    Ok(config)
}

fn load_config_from_env() -> anyhow::Result<ServerConfig> {
    let mut config = ServerConfig::default();
    if let Ok(host) = std::env::var("REALTIME_HOST") {
        config.host = host;
    }
    if let Ok(port) = std::env::var("REALTIME_PORT") {
        config.port = port.parse()?;
    }
    if let Ok(secret) = std::env::var("REALTIME_JWT_SECRET") {
        config.auth = AuthConfig::Jwt {
            secret,
            issuer: std::env::var("REALTIME_JWT_ISSUER").ok(),
            audience: std::env::var("REALTIME_JWT_AUDIENCE").ok(),
        };
    }
    Ok(config)
}

fn add_pg_config(config: &mut ServerConfig) {
    let Ok(pg_url) = std::env::var("REALTIME_PG_URL") else {
        return;
    };
    let channel =
        std::env::var("REALTIME_PG_CHANNEL").unwrap_or_else(|_| "realtime_events".to_string());
    let prefix = std::env::var("REALTIME_PG_PREFIX").unwrap_or_else(|_| "pg".to_string());
    config.databases.push(DatabaseConfig {
        adapter: "postgresql".to_string(),
        config: serde_json::json!({
            "connection_string": pg_url,
            "channel": channel,
            "tables": [],
            "topic_prefix": prefix,
            "poll_interval_ms": 100
        }),
    });
    tracing::info!("PostgreSQL CDC configured from env");
}

fn add_mongo_config(config: &mut ServerConfig) {
    let Ok(uri) = std::env::var("REALTIME_MONGO_URI") else {
        return;
    };
    let database = std::env::var("REALTIME_MONGO_DB").unwrap_or_else(|_| "syncspace".to_string());
    let prefix = std::env::var("REALTIME_MONGO_PREFIX").unwrap_or_else(|_| "mongo".to_string());
    config.databases.push(DatabaseConfig {
        adapter: "mongodb".to_string(),
        config: serde_json::json!({
            "uri": uri,
            "database": database,
            "collections": [],
            "topic_prefix": prefix,
            "full_document": "updateLookup"
        }),
    });
    tracing::info!("MongoDB Change Streams configured from env");
}

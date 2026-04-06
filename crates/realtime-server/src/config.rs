/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   config.rs                                          :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:13:39 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:19:25 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! Server configuration types.
//!
//! The configuration can be loaded from a JSON file (via `REALTIME_CONFIG`
//! env var) or assembled from individual environment variables.
//!
//! ## Defaults
//!
//! | Field | Default |
//! |---|---|
//! | `host` | `0.0.0.0` |
//! | `port` | `9090` |
//! | `event_bus` | in-process, 65 536 capacity |
//! | `auth` | no-auth |
//! | `send_queue_capacity` | 256 |
//! | `fanout_workers` | number of CPUs |
//! | `dispatch_channel_capacity` | 65 536 |

use serde::{Deserialize, Serialize};

/// Top-level server configuration.
///
/// Loaded from JSON or built programmatically. All sections have sensible
/// defaults so an empty `{}` JSON file produces a working server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Server host address.
    #[serde(default = "default_host")]
    pub host: String,

    /// Server port.
    #[serde(default = "default_port")]
    pub port: u16,

    /// Event bus configuration.
    #[serde(default)]
    pub event_bus: EventBusConfig,

    /// Authentication configuration.
    #[serde(default)]
    pub auth: AuthConfig,

    /// Performance tuning.
    #[serde(default)]
    pub performance: PerformanceConfig,

    /// Database producers.
    #[serde(default)]
    pub databases: Vec<DatabaseConfig>,
}

/// Event bus backend selection.
///
/// Currently only `InProcess` is supported. Future variants could include
/// Redis Streams, NATS JetStream, or Apache Kafka.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventBusConfig {
    #[serde(rename = "inprocess")]
    InProcess {
        #[serde(default = "default_bus_capacity")]
        capacity: usize,
    },
}

impl Default for EventBusConfig {
    fn default() -> Self {
        Self::InProcess {
            capacity: default_bus_capacity(),
        }
    }
}

/// Authentication backend selection.
///
/// - `NoAuth` — accepts all tokens (development only).
/// - `Jwt` — validates HMAC-SHA256 / RSA tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AuthConfig {
    #[serde(rename = "none")]
    NoAuth,
    #[serde(rename = "jwt")]
    Jwt {
        secret: String,
        #[serde(default)]
        issuer: Option<String>,
        #[serde(default)]
        audience: Option<String>,
    },
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self::NoAuth
    }
}

/// Performance tuning knobs.
///
/// All fields have sensible defaults for typical workloads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Send queue capacity per connection.
    #[serde(default = "default_send_queue")]
    pub send_queue_capacity: usize,

    /// Number of fan-out worker tasks.
    #[serde(default = "default_fanout_workers")]
    pub fanout_workers: usize,

    /// Dispatch channel capacity.
    #[serde(default = "default_dispatch_capacity")]
    pub dispatch_channel_capacity: usize,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            send_queue_capacity: default_send_queue(),
            fanout_workers: default_fanout_workers(),
            dispatch_channel_capacity: default_dispatch_capacity(),
        }
    }
}

/// Generic database producer configuration.
///
/// Each entry names an adapter (e.g. `"postgresql"`, `"mongodb"`) and
/// provides adapter-specific JSON config that is passed to the
/// [`ProducerFactory`](realtime_core::ProducerFactory) at runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// Adapter name: "postgresql", "mongodb", "mysql", "redis", etc.
    pub adapter: String,
    /// Adapter-specific configuration (passed as JSON to the ProducerFactory).
    #[serde(default)]
    pub config: serde_json::Value,
}

/// Legacy enum kept for backward compatibility with old JSON configs.
/// Automatically converts to the new generic DatabaseConfig.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LegacyDatabaseConfig {
    #[serde(rename = "postgresql")]
    PostgreSQL(serde_json::Value),
    #[serde(rename = "mongodb")]
    MongoDB(serde_json::Value),
}

impl From<LegacyDatabaseConfig> for DatabaseConfig {
    fn from(legacy: LegacyDatabaseConfig) -> Self {
        match legacy {
            LegacyDatabaseConfig::PostgreSQL(config) => DatabaseConfig {
                adapter: "postgresql".to_string(),
                config,
            },
            LegacyDatabaseConfig::MongoDB(config) => DatabaseConfig {
                adapter: "mongodb".to_string(),
                config,
            },
        }
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    9090
}

fn default_bus_capacity() -> usize {
    65536
}

fn default_send_queue() -> usize {
    256
}

fn default_fanout_workers() -> usize {
    num_cpus()
}

fn default_dispatch_capacity() -> usize {
    65536
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4)
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            event_bus: EventBusConfig::default(),
            auth: AuthConfig::default(),
            performance: PerformanceConfig::default(),
            databases: vec![],
        }
    }
}

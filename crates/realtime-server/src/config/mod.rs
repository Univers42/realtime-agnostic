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

mod auth;
mod bus;
mod database;
mod performance;

pub use auth::AuthConfig;
pub use bus::EventBusConfig;
pub use database::{DatabaseConfig, LegacyDatabaseConfig};
pub use performance::PerformanceConfig;

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

fn default_host() -> String {
    "0.0.0.0".to_string()
}

const fn default_port() -> u16 {
    9090
}

//! Authentication configuration.

use serde::{Deserialize, Serialize};

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

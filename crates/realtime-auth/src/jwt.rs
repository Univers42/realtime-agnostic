/* ************************************************************************** */
/*                                                                            */
/*                                                        :::      ::::::::   */
/*   jwt.rs                                             :+:      :+:    :+:   */
/*                                                    +:+ +:+         +:+     */
/*   By: dlesieur <dlesieur@student.42.fr>          +#+  +:+       +#+        */
/*                                                +#+#+#+#+#+   +#+           */
/*   Created: 2026/04/07 11:12:09 by dlesieur          #+#    #+#             */
/*   Updated: 2026/04/07 11:17:52 by dlesieur         ###   ########.fr       */
/*                                                                            */
/* ************************************************************************** */

//! JWT-based authentication provider using HMAC-SHA256 (or RSA).
//!
//! Verifies JSON Web Tokens and extracts [`AuthClaims`] for authorization.
//! Supports `Bearer` prefix stripping, configurable issuer/audience
//! validation, and namespace-based access control.
//!
//! ## Token format
//!
//! ```json
//! {
//!   "sub": "user-123",
//!   "exp": 1719999999,
//!   "namespaces": ["orders", "users"],
//!   "can_publish": true,
//!   "can_subscribe": true,
//!   "metadata": {}
//! }
//! ```

use std::collections::HashMap;

use async_trait::async_trait;
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use realtime_core::{AuthClaims, AuthContext, AuthProvider, RealtimeError, Result, TopicPath, TopicPattern};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// JWT authentication provider.
///
/// Verifies tokens using HMAC-SHA256 (or RSA if configured) and
/// extracts [`AuthClaims`] for namespace-based authorization.
pub struct JwtAuthProvider {
    /// Pre-computed decoding key.
    decoding_key: DecodingKey,
    /// Validation settings (algorithm, issuer, audience).
    validation: Validation,
}

/// Internal JWT claims structure expected in the token payload.
///
/// Deserialized from the JWT; then mapped to [`AuthClaims`].
#[derive(Debug, Serialize, Deserialize)]
struct JwtClaims {
    /// Subject (user ID)
    sub: String,
    /// Expiration time (Unix timestamp)
    exp: Option<u64>,
    /// Issued at
    iat: Option<u64>,
    /// Allowed namespaces
    #[serde(default)]
    namespaces: Vec<String>,
    /// Can publish
    #[serde(default)]
    can_publish: bool,
    /// Can subscribe
    #[serde(default = "default_true")]
    can_subscribe: bool,
    /// Custom metadata
    #[serde(default)]
    metadata: HashMap<String, serde_json::Value>,
}

fn default_true() -> bool {
    true
}

/// Configuration for the JWT auth provider.
///
/// Use [`JwtConfig::hmac()`] for the common HMAC-SHA256 setup.
pub struct JwtConfig {
    /// HMAC secret string or RSA PEM-encoded public key.
    pub secret: String,
    /// JWT algorithm (default: HS256).
    pub algorithm: Algorithm,
    /// Expected `iss` claim (optional).
    pub issuer: Option<String>,
    /// Expected `aud` claim (optional).
    pub audience: Option<String>,
}

impl JwtConfig {
    /// Create a simple HMAC-SHA256 JWT config.
    ///
    /// # Arguments
    ///
    /// * `secret` — The shared HMAC secret (should be ≥32 bytes).
    pub fn hmac(secret: impl Into<String>) -> Self {
        Self {
            secret: secret.into(),
            algorithm: Algorithm::HS256,
            issuer: None,
            audience: None,
        }
    }
}

impl JwtAuthProvider {
    /// Create a new JWT auth provider from config.
    ///
    /// Panics if the RSA PEM key is invalid (for RSA algorithms).
    pub fn new(config: JwtConfig) -> Self {
        let decoding_key = match config.algorithm {
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
                DecodingKey::from_secret(config.secret.as_bytes())
            }
            _ => {
                // For RSA/EC, the secret is the PEM-encoded public key
                DecodingKey::from_rsa_pem(config.secret.as_bytes())
                    .expect("Invalid RSA public key PEM")
            }
        };

        let mut validation = Validation::new(config.algorithm);
        if let Some(issuer) = config.issuer {
            validation.set_issuer(&[issuer]);
        }
        if let Some(audience) = config.audience {
            validation.set_audience(&[audience]);
        }

        Self {
            decoding_key,
            validation,
        }
    }
}

#[async_trait]
impl AuthProvider for JwtAuthProvider {
    async fn verify(&self, token: &str, _context: &AuthContext) -> Result<AuthClaims> {
        // Strip "Bearer " prefix if present
        let token = token.strip_prefix("Bearer ").unwrap_or(token);

        let token_data = decode::<JwtClaims>(token, &self.decoding_key, &self.validation)
            .map_err(|e| {
                warn!("JWT verification failed: {}", e);
                RealtimeError::AuthFailed(format!("Invalid token: {}", e))
            })?;

        let claims = token_data.claims;
        debug!(sub = %claims.sub, "JWT verified successfully");

        Ok(AuthClaims {
            sub: claims.sub,
            namespaces: if claims.namespaces.is_empty() {
                vec!["*".to_string()]
            } else {
                claims.namespaces
            },
            can_publish: claims.can_publish,
            can_subscribe: claims.can_subscribe,
            metadata: claims.metadata,
        })
    }

    async fn authorize_subscribe(&self, claims: &AuthClaims, topic: &TopicPattern) -> Result<()> {
        if claims.can_subscribe_to(topic) {
            Ok(())
        } else {
            Err(RealtimeError::AuthorizationDenied(format!(
                "Not authorized to subscribe to {}",
                topic
            )))
        }
    }

    async fn authorize_publish(&self, claims: &AuthClaims, topic: &TopicPath) -> Result<()> {
        if claims.can_publish_to(topic) {
            Ok(())
        } else {
            Err(RealtimeError::AuthorizationDenied(format!(
                "Not authorized to publish to {}",
                topic
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn create_test_jwt(claims: &JwtClaims, secret: &str) -> String {
        let key = EncodingKey::from_secret(secret.as_bytes());
        encode(&Header::default(), claims, &key).unwrap()
    }

    #[tokio::test]
    async fn test_jwt_verify_valid_token() {
        let secret = "test-secret-key-at-least-32-chars!!";
        let provider = JwtAuthProvider::new(JwtConfig::hmac(secret));

        let jwt_claims = JwtClaims {
            sub: "user-123".to_string(),
            exp: Some(chrono::Utc::now().timestamp() as u64 + 3600),
            iat: Some(chrono::Utc::now().timestamp() as u64),
            namespaces: vec!["orders".to_string()],
            can_publish: true,
            can_subscribe: true,
            metadata: HashMap::new(),
        };

        let token = create_test_jwt(&jwt_claims, secret);

        let ctx = AuthContext {
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            transport: "ws".to_string(),
        };

        let claims = provider.verify(&token, &ctx).await.unwrap();
        assert_eq!(claims.sub, "user-123");
        assert!(claims.can_publish);
        assert!(claims.can_subscribe);
        assert!(claims.namespaces.contains(&"orders".to_string()));
    }

    #[tokio::test]
    async fn test_jwt_verify_invalid_token() {
        let provider = JwtAuthProvider::new(JwtConfig::hmac("secret-key-32-chars-minimum!!!!"));

        let ctx = AuthContext {
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            transport: "ws".to_string(),
        };

        let result = provider.verify("invalid-token", &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_jwt_verify_with_bearer_prefix() {
        let secret = "test-secret-key-at-least-32-chars!!";
        let provider = JwtAuthProvider::new(JwtConfig::hmac(secret));

        let jwt_claims = JwtClaims {
            sub: "user-456".to_string(),
            exp: Some(chrono::Utc::now().timestamp() as u64 + 3600),
            iat: None,
            namespaces: vec![],
            can_publish: false,
            can_subscribe: true,
            metadata: HashMap::new(),
        };

        let token = format!("Bearer {}", create_test_jwt(&jwt_claims, secret));

        let ctx = AuthContext {
            peer_addr: "127.0.0.1:12345".parse().unwrap(),
            transport: "ws".to_string(),
        };

        let claims = provider.verify(&token, &ctx).await.unwrap();
        assert_eq!(claims.sub, "user-456");
        assert!(!claims.can_publish);
    }

    #[tokio::test]
    async fn test_jwt_authorize_subscribe() {
        let secret = "test-secret-key-at-least-32-chars!!";
        let provider = JwtAuthProvider::new(JwtConfig::hmac(secret));

        let claims = AuthClaims {
            sub: "user1".to_string(),
            namespaces: vec!["orders".to_string()],
            can_publish: false,
            can_subscribe: true,
            metadata: HashMap::new(),
        };

        let topic_ok = TopicPattern::parse("orders/created");
        let topic_denied = TopicPattern::parse("admin/settings");

        assert!(provider.authorize_subscribe(&claims, &topic_ok).await.is_ok());
        assert!(provider.authorize_subscribe(&claims, &topic_denied).await.is_err());
    }
}

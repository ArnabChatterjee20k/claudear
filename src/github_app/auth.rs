//! JWT authentication for GitHub Apps.
//!
//! This module handles:
//! - JWT generation for authenticating as a GitHub App
//! - Installation token retrieval and caching
//!
//! GitHub App authentication uses a two-step process:
//! 1. Generate a JWT signed with the App's private key
//! 2. Exchange the JWT for an installation access token
//!
//! Installation tokens are short-lived (1 hour) and scoped to a specific installation.

use crate::config::GitHubAppConfig;
use crate::error::{Error, Result};
use chrono::{Duration, Utc};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Buffer time (in minutes) before token expiry to request a new token.
/// This ensures we don't use tokens that are about to expire.
const TOKEN_CACHE_BUFFER_MINUTES: i64 = 5;

/// JWT claims for GitHub App authentication.
#[derive(Debug, Serialize)]
struct JwtClaims {
    /// Issued at time (Unix timestamp).
    iat: i64,
    /// Expiration time (Unix timestamp). Maximum 10 minutes from iat.
    exp: i64,
    /// GitHub App ID (issuer).
    iss: String,
}

/// Cached installation token with expiration.
#[derive(Debug, Clone)]
pub struct CachedToken {
    /// The installation access token.
    pub token: String,
    /// When the token expires.
    pub expires_at: chrono::DateTime<Utc>,
}

impl CachedToken {
    /// Check if the token is still valid (with buffer before expiry).
    pub fn is_valid(&self) -> bool {
        Utc::now() + Duration::minutes(TOKEN_CACHE_BUFFER_MINUTES) < self.expires_at
    }
}

/// Installation token response from GitHub API.
#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: String,
}

/// GitHub App JWT generator and token cache.
#[derive(Debug)]
pub struct GitHubAppAuth {
    config: GitHubAppConfig,
    /// Cache of installation tokens by installation ID.
    token_cache: Arc<RwLock<HashMap<i64, CachedToken>>>,
}

impl GitHubAppAuth {
    /// Create a new GitHubAppAuth from configuration.
    pub fn new(config: GitHubAppConfig) -> Self {
        Self {
            config,
            token_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Generate a JWT for authenticating as the GitHub App.
    ///
    /// The JWT is valid for 10 minutes (maximum allowed by GitHub).
    pub fn generate_jwt(&self) -> Result<String> {
        let app_id = self
            .config
            .app_id
            .ok_or_else(|| Error::config("GitHub App ID not configured"))?;

        let private_key_pem = self.config.load_private_key()?;

        let now = Utc::now();
        // GitHub recommends setting iat to 60 seconds in the past to account for clock drift
        let iat = (now - Duration::seconds(60)).timestamp();
        // Maximum expiration is 10 minutes
        let exp = (now + Duration::minutes(10)).timestamp();

        let claims = JwtClaims {
            iat,
            exp,
            iss: app_id.to_string(),
        };

        let header = Header::new(Algorithm::RS256);
        let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
            .map_err(|e| Error::config(format!("Invalid RSA private key: {}", e)))?;

        encode(&header, &claims, &encoding_key)
            .map_err(|e| Error::config(format!("Failed to encode JWT: {}", e)))
    }

    /// Get the App ID from config.
    pub fn app_id(&self) -> Option<i64> {
        self.config.app_id
    }

    /// Get the configured installation ID (if set).
    pub fn installation_id(&self) -> Option<i64> {
        self.config.installation_id
    }

    /// Get a cached installation token if it's still valid.
    pub fn get_cached_token(&self, installation_id: i64) -> Option<CachedToken> {
        let cache = self.token_cache.read().ok()?;
        cache
            .get(&installation_id)
            .filter(|t| t.is_valid())
            .cloned()
    }

    /// Cache an installation token.
    pub fn cache_token(&self, installation_id: i64, token: CachedToken) {
        if let Ok(mut cache) = self.token_cache.write() {
            cache.insert(installation_id, token);
        }
    }

    /// Clear the token cache for a specific installation.
    pub fn clear_cached_token(&self, installation_id: i64) {
        if let Ok(mut cache) = self.token_cache.write() {
            cache.remove(&installation_id);
        }
    }

    /// Clear all cached tokens.
    pub fn clear_all_tokens(&self) {
        if let Ok(mut cache) = self.token_cache.write() {
            cache.clear();
        }
    }

    /// Parse an installation token response from GitHub.
    pub fn parse_token_response(&self, response_body: &str) -> Result<CachedToken> {
        let response: InstallationTokenResponse =
            serde_json::from_str(response_body).map_err(|e| {
                Error::config(format!(
                    "Failed to parse installation token response: {}",
                    e
                ))
            })?;

        let expires_at = chrono::DateTime::parse_from_rfc3339(&response.expires_at)
            .map_err(|e| Error::config(format!("Failed to parse token expiration: {}", e)))?
            .with_timezone(&Utc);

        Ok(CachedToken {
            token: response.token,
            expires_at,
        })
    }

    /// Get the URL for requesting an installation access token.
    pub fn installation_token_url(&self, installation_id: i64) -> String {
        format!(
            "https://api.github.com/app/installations/{}/access_tokens",
            installation_id
        )
    }

    /// Build headers for GitHub App JWT authentication.
    pub fn jwt_headers(&self) -> Result<Vec<(&'static str, String)>> {
        let jwt = self.generate_jwt()?;
        Ok(vec![
            ("Authorization", format!("Bearer {}", jwt)),
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ])
    }

    /// Build headers for installation token authentication.
    pub fn token_headers(token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("token {}", token)),
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretValue;

    // Test RSA private key (for testing only - NOT a real key, generated for testing)
    const TEST_PRIVATE_KEY: &str = r#"-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEAj+XxUhUTsewiFEZe6f6CBEITupuaaBsU7pgZrHqtb8rnlH1A
sgaE03hKaxUUO/7rlwJkHuG0orhH3WQmctW8CiCaPSZE1aJrbCOJRFQg8aQmv+Lv
SgizJ69nQsdXkuTT/MzUypZeAI89dvkfhNznLjwaessTXFIKeOGsV7jWYnJOrf7X
kn1rLpZQ2IawCaqo9cRQumMx8uHZD4AiYmPIHd07za4hmxcwGD3ulMlyCII3a+os
3Mj2NVqegqQ2gKgd782Hp2shMf57do4RKOy+6S9rJV92PR51EhuuIQu2ZL8ijNg1
C4tkpO+5axrgh8UeXIVgOKgofqEmSXjYMj4D7QIDAQABAoIBAAhBeQbsjqS2l33y
S5/BKlR0Ng2Ov90ZMKo/r7llkG3Jhl/Oj9em6Bf53ssl+nM2vO19BaF/8Y0kZXse
M9aCzLcIB9FaULixCNi7cTSqXvl+IXsA2hm1RhIQzivWo/+ZgVAPsGWvGtWNYklh
IZ3NzrWoXRyOah3x1wf4aprdz+716ecYEnMUHHSJ+hpR8ZNGeek7QNc9RoP7Rm5I
NEcnvPJCtmqOnGXxDzOOotR1dpG8rY0txkv3ayobFJFq3h8/RMLPANxHlE7k8HRu
Ow9fFgg5wDNJCj7wj8l+t9wduoRhs77P/42rSsePktpnjG/iNgyaM3bJopMvEgyo
ahahXUECgYEAwrzRTQE1hy7EAR9FtAzrt3sa1RV2egbsD8OWspghDiSQ3mXFCz95
viJk5AxjS9NudgRBc5/LaiYv7Y4tmdhTDS5G+Z30ObHySRUV77/1JLGlAKxUacN6
Ixz3wP88WVzdB1ALiAzrwJV+2w1m43x2lM4nMg26mFPBQabkc+FT22sCgYEAvSrH
lLOxlyewojj6dI0VMcV59GTH1T+4Erjw405YPAQ3E4PkQZ9UG5Yhl/BpwX7uxAB8
6ZinAIRkhIeQybjZsdcOeakGqPc/cOz/Pvxt+ZU9lj/EaIcRV88dTEIcQacv/LNI
+Wikgy9qtng6mn7fBLwaoql9z5EpnQctiFR+DAcCgYEAoWLsLl4nJ145cBijoqDm
pMuwJBHCe0TLVBErDd2H33msWbOLxlOXqFxGsrwVepzBuaqzN4ihgtoc9EnVPt+J
jK3igjJGWZ5AhhKkeGnkVsGmVlV7K5+l0/3I0bh1IjYUs1/B/sF+i78ZP57uuu7G
M3JaB2BbWKxox+jxAZwm6/sCgYAC95zR1E/A0zqOEN683Umr0jEriDkqOymkAYql
xiDUMCy8/aCi9uDW3fAA9iByjI8qO+e5sk9MTsdU3NuEjoW7qGftuJ0GIXq5Rr5q
OoNvGswwgyeNjDDVc8Y93/uZfAngqN9IKkAKXsAJxLEGo17UMC8qxgXXL6u7btVk
Ag9IGQKBgQChUUTkc/oGYPdCdfpYR9y9+9bBVvTaIYA+5gi1jbUUQOcKzSMpW9yt
5QIzidGZB0BWLV/Bys6EpQMCPUQHoDtaqev2frR4Ug8MPYWTtxoBBtZ3Inqpaz/5
zvWGmeHev+iEP/vneCazbHGQpeC1zFX+P+tQr/zhl1klmnSGl6Zs3w==
-----END RSA PRIVATE KEY-----"#;

    fn create_test_config(with_key: bool) -> GitHubAppConfig {
        GitHubAppConfig {
            app_id: Some(12345),
            private_key: if with_key {
                Some(SecretValue::new(TEST_PRIVATE_KEY))
            } else {
                None
            },
            private_key_path: None,
            webhook_secret: Some("test_secret".into()),
            installation_id: Some(67890),
            client_id: Some("Iv1.abc123".to_string()),
            client_secret: Some("secret".into()),
            base_url: Some("https://example.com".to_string()),
        }
    }

    #[test]
    fn test_generate_jwt() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        let jwt = auth.generate_jwt().unwrap();
        assert!(!jwt.is_empty());

        // JWT should have 3 parts separated by dots
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
    }

    #[test]
    fn test_generate_jwt_missing_app_id() {
        let mut config = create_test_config(true);
        config.app_id = None;
        let auth = GitHubAppAuth::new(config);

        let result = auth.generate_jwt();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("App ID"));
    }

    #[test]
    fn test_generate_jwt_missing_private_key() {
        let config = create_test_config(false);
        let auth = GitHubAppAuth::new(config);

        let result = auth.generate_jwt();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("private key"));
    }

    #[test]
    fn test_token_cache() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        // No cached token initially
        assert!(auth.get_cached_token(67890).is_none());

        // Cache a token
        let token = CachedToken {
            token: "test_token".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };
        auth.cache_token(67890, token.clone());

        // Should be able to retrieve it
        let cached = auth.get_cached_token(67890).unwrap();
        assert_eq!(cached.token, "test_token");
        assert!(cached.is_valid());

        // Clear it
        auth.clear_cached_token(67890);
        assert!(auth.get_cached_token(67890).is_none());
    }

    #[test]
    fn test_token_cache_expired() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        // Cache an expired token
        let token = CachedToken {
            token: "expired_token".to_string(),
            expires_at: Utc::now() - Duration::hours(1),
        };
        auth.cache_token(67890, token);

        // Should not be retrievable (expired)
        assert!(auth.get_cached_token(67890).is_none());
    }

    #[test]
    fn test_cached_token_is_valid() {
        // Valid token (expires in 1 hour)
        let valid_token = CachedToken {
            token: "valid".to_string(),
            expires_at: Utc::now() + Duration::hours(1),
        };
        assert!(valid_token.is_valid());

        // Token expiring in 4 minutes (less than 5 minute buffer)
        let expiring_soon = CachedToken {
            token: "expiring".to_string(),
            expires_at: Utc::now() + Duration::minutes(4),
        };
        assert!(!expiring_soon.is_valid());

        // Expired token
        let expired = CachedToken {
            token: "expired".to_string(),
            expires_at: Utc::now() - Duration::minutes(1),
        };
        assert!(!expired.is_valid());
    }

    #[test]
    fn test_parse_token_response() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        let response = r#"{"token": "ghs_test_token", "expires_at": "2024-01-15T12:00:00Z"}"#;
        let cached = auth.parse_token_response(response).unwrap();

        assert_eq!(cached.token, "ghs_test_token");
    }

    #[test]
    fn test_installation_token_url() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        let url = auth.installation_token_url(12345);
        assert_eq!(
            url,
            "https://api.github.com/app/installations/12345/access_tokens"
        );
    }

    #[test]
    fn test_jwt_headers() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        let headers = auth.jwt_headers().unwrap();
        assert_eq!(headers.len(), 3);
        assert!(headers[0].1.starts_with("Bearer "));
    }

    #[test]
    fn test_token_headers() {
        let headers = GitHubAppAuth::token_headers("test_token");
        assert_eq!(headers.len(), 3);
        assert_eq!(headers[0].1, "token test_token");
    }

    #[test]
    fn test_clear_all_tokens() {
        let config = create_test_config(true);
        let auth = GitHubAppAuth::new(config);

        // Cache some tokens
        for i in 1..=5 {
            let token = CachedToken {
                token: format!("token_{}", i),
                expires_at: Utc::now() + Duration::hours(1),
            };
            auth.cache_token(i, token);
        }

        // Verify they exist
        assert!(auth.get_cached_token(1).is_some());
        assert!(auth.get_cached_token(5).is_some());

        // Clear all
        auth.clear_all_tokens();

        // Verify they're gone
        assert!(auth.get_cached_token(1).is_none());
        assert!(auth.get_cached_token(5).is_none());
    }
}

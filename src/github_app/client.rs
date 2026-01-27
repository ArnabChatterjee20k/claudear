//! GitHub App API client.
//!
//! This client provides methods for interacting with the GitHub API
//! as a GitHub App, including:
//!
//! - Listing installations
//! - Getting installation access tokens
//! - Making authenticated API requests

use crate::config::GitHubAppConfig;
use crate::error::{Error, Result};
use crate::github_app::auth::{CachedToken, GitHubAppAuth};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Default timeout for HTTP requests (30 seconds).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default connection timeout (10 seconds).
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 10;

/// Installation information from GitHub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installation {
    /// Installation ID.
    pub id: i64,
    /// Account that installed the App.
    pub account: InstallationAccount,
    /// Target type (User or Organization).
    pub target_type: String,
    /// Repository selection mode.
    pub repository_selection: String,
    /// App ID this installation belongs to.
    pub app_id: i64,
    /// HTML URL for the installation settings.
    pub html_url: Option<String>,
}

/// Account information for an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationAccount {
    /// Account login (username or org name).
    pub login: String,
    /// Account ID.
    pub id: i64,
    /// Account type (User or Organization).
    #[serde(rename = "type")]
    pub account_type: String,
}

/// Repository information from an installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationRepository {
    /// Repository ID.
    pub id: i64,
    /// Repository name.
    pub name: String,
    /// Full repository name (owner/repo).
    pub full_name: String,
    /// Whether the repository is private.
    pub private: bool,
}

/// Response from listing installation repositories.
#[derive(Debug, Deserialize)]
struct InstallationReposResponse {
    #[allow(dead_code)]
    total_count: i64,
    repositories: Vec<InstallationRepository>,
}

/// Client for GitHub App API operations.
#[derive(Debug)]
pub struct GitHubAppClient {
    auth: GitHubAppAuth,
    http_client: reqwest::Client,
}

impl GitHubAppClient {
    /// Create a new GitHubAppClient from configuration.
    pub fn new(config: GitHubAppConfig) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            auth: GitHubAppAuth::new(config),
            http_client,
        }
    }

    /// Create a new GitHubAppClient with a custom HTTP client.
    pub fn with_http_client(config: GitHubAppConfig, client: reqwest::Client) -> Self {
        Self {
            auth: GitHubAppAuth::new(config),
            http_client: client,
        }
    }

    /// Get the underlying auth handler.
    pub fn auth(&self) -> &GitHubAppAuth {
        &self.auth
    }

    /// Get the App ID.
    pub fn app_id(&self) -> Option<i64> {
        self.auth.app_id()
    }

    /// List all installations for this App.
    pub async fn list_installations(&self) -> Result<Vec<Installation>> {
        let headers = self.auth.jwt_headers()?;

        let mut request = self
            .http_client
            .get("https://api.github.com/app/installations");

        for (name, value) in headers {
            request = request.header(name, value);
        }
        request = request.header("User-Agent", "claudear");

        let response = request
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to list installations: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub API error ({}): {}",
                status, body
            )));
        }

        let installations: Vec<Installation> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse installations: {}", e)))?;

        Ok(installations)
    }

    /// Get an installation access token.
    ///
    /// This method caches tokens and only requests new ones when needed.
    pub async fn get_installation_token(&self, installation_id: i64) -> Result<String> {
        // Check cache first
        if let Some(cached) = self.auth.get_cached_token(installation_id) {
            return Ok(cached.token);
        }

        // Request new token
        let token = self.request_installation_token(installation_id).await?;

        // Cache it
        self.auth.cache_token(installation_id, token.clone());

        Ok(token.token)
    }

    /// Request a new installation token from GitHub.
    async fn request_installation_token(&self, installation_id: i64) -> Result<CachedToken> {
        let url = self.auth.installation_token_url(installation_id);
        let headers = self.auth.jwt_headers()?;

        let mut request = self.http_client.post(&url);

        for (name, value) in headers {
            request = request.header(name, value);
        }
        request = request.header("User-Agent", "claudear");

        let response = request
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to get installation token: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub API error ({}): {}",
                status, body
            )));
        }

        let body = response
            .text()
            .await
            .map_err(|e| Error::api(format!("Failed to read response: {}", e)))?;

        self.auth.parse_token_response(&body)
    }

    /// List repositories accessible to an installation.
    pub async fn list_installation_repos(
        &self,
        installation_id: i64,
    ) -> Result<Vec<InstallationRepository>> {
        let token = self.get_installation_token(installation_id).await?;
        let headers = GitHubAppAuth::token_headers(&token);

        let mut request = self
            .http_client
            .get("https://api.github.com/installation/repositories");

        for (name, value) in headers {
            request = request.header(name, value);
        }
        request = request.header("User-Agent", "claudear");

        let response = request
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to list repositories: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub API error ({}): {}",
                status, body
            )));
        }

        let repos_response: InstallationReposResponse = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse repositories: {}", e)))?;

        Ok(repos_response.repositories)
    }

    /// Find the installation for a specific repository.
    pub async fn find_installation_for_repo(&self, owner: &str, repo: &str) -> Result<i64> {
        let installations = self.list_installations().await?;

        for installation in installations {
            // Check if this installation has access to the repo
            let repos = self.list_installation_repos(installation.id).await?;
            let full_name = format!("{}/{}", owner, repo);

            if repos.iter().any(|r| r.full_name == full_name) {
                return Ok(installation.id);
            }
        }

        Err(Error::api(format!(
            "No installation found with access to {}/{}",
            owner, repo
        )))
    }

    /// Get the configured installation ID, or find one automatically.
    pub async fn get_or_find_installation(&self, owner: &str, repo: &str) -> Result<i64> {
        // If installation ID is configured, use it
        if let Some(id) = self.auth.installation_id() {
            return Ok(id);
        }

        // Otherwise, try to find it
        self.find_installation_for_repo(owner, repo).await
    }

    /// Make an authenticated API request using an installation token.
    pub async fn api_request(
        &self,
        installation_id: i64,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let token = self.get_installation_token(installation_id).await?;
        let headers = GitHubAppAuth::token_headers(&token);

        let mut request = self.http_client.request(method, url);

        for (name, value) in headers {
            request = request.header(name, value);
        }
        request = request.header("User-Agent", "claudear");

        if let Some(body) = body {
            request = request.json(&body);
        }

        let response = request
            .send()
            .await
            .map_err(|e| Error::api(format!("API request failed: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub API error ({}): {}",
                status, body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse response: {}", e)))
    }

    /// Get information about the authenticated App.
    pub async fn get_app_info(&self) -> Result<serde_json::Value> {
        let headers = self.auth.jwt_headers()?;

        let mut request = self.http_client.get("https://api.github.com/app");

        for (name, value) in headers {
            request = request.header(name, value);
        }
        request = request.header("User-Agent", "claudear");

        let response = request
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to get app info: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Error::api(format!(
                "GitHub API error ({}): {}",
                status, body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse app info: {}", e)))
    }

    /// Invalidate cached token for an installation.
    pub fn invalidate_token(&self, installation_id: i64) {
        self.auth.clear_cached_token(installation_id);
    }

    /// Clear all cached tokens.
    pub fn clear_token_cache(&self) {
        self.auth.clear_all_tokens();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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

    fn create_test_config() -> GitHubAppConfig {
        GitHubAppConfig {
            app_id: Some(12345),
            private_key: Some(TEST_PRIVATE_KEY.to_string()),
            private_key_path: None,
            webhook_secret: Some("test_secret".to_string()),
            installation_id: Some(67890),
            client_id: Some("Iv1.abc123".to_string()),
            client_secret: Some("secret".to_string()),
            base_url: Some("https://example.com".to_string()),
        }
    }

    #[test]
    fn test_client_creation() {
        let config = create_test_config();
        let client = GitHubAppClient::new(config);
        assert_eq!(client.app_id(), Some(12345));
    }

    #[test]
    fn test_client_with_http_client() {
        let config = create_test_config();
        let http = reqwest::Client::new();
        let client = GitHubAppClient::with_http_client(config, http);
        assert_eq!(client.app_id(), Some(12345));
    }

    #[test]
    fn test_installation_serialization() {
        let json = r#"{
            "id": 12345,
            "account": {
                "login": "octocat",
                "id": 1,
                "type": "User"
            },
            "target_type": "User",
            "repository_selection": "all",
            "app_id": 67890,
            "html_url": "https://github.com/settings/installations/12345"
        }"#;

        let installation: Installation = serde_json::from_str(json).unwrap();
        assert_eq!(installation.id, 12345);
        assert_eq!(installation.account.login, "octocat");
        assert_eq!(installation.target_type, "User");
    }

    #[test]
    fn test_installation_repo_serialization() {
        let json = r#"{
            "id": 1,
            "name": "repo",
            "full_name": "owner/repo",
            "private": false
        }"#;

        let repo: InstallationRepository = serde_json::from_str(json).unwrap();
        assert_eq!(repo.id, 1);
        assert_eq!(repo.name, "repo");
        assert_eq!(repo.full_name, "owner/repo");
        assert!(!repo.private);
    }

    #[test]
    fn test_token_caching() {
        let config = create_test_config();
        let client = GitHubAppClient::new(config);

        // Initially no cached token
        assert!(client.auth().get_cached_token(67890).is_none());

        // Cache a token
        let token = CachedToken {
            token: "test_token".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
        };
        client.auth().cache_token(67890, token);

        // Should be cached
        assert!(client.auth().get_cached_token(67890).is_some());

        // Invalidate
        client.invalidate_token(67890);
        assert!(client.auth().get_cached_token(67890).is_none());
    }

    #[test]
    fn test_clear_token_cache() {
        let config = create_test_config();
        let client = GitHubAppClient::new(config);

        // Cache multiple tokens
        for i in 1..=5 {
            let token = CachedToken {
                token: format!("token_{}", i),
                expires_at: Utc::now() + chrono::Duration::hours(1),
            };
            client.auth().cache_token(i, token);
        }

        // Clear all
        client.clear_token_cache();

        // All should be gone
        for i in 1..=5 {
            assert!(client.auth().get_cached_token(i).is_none());
        }
    }
}

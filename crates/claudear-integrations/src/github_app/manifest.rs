//! GitHub App manifest generation for the setup flow.
//!
//! This module generates the JSON manifest that GitHub uses to pre-fill
//! the App creation form during the manifest flow.
//!
//! See: https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest

use serde::{Deserialize, Serialize};

/// GitHub App permissions.
///
/// These are the permissions requested by the App during installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppPermissions {
    /// Permission level for pull requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<String>,
    /// Permission level for repository contents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contents: Option<String>,
    /// Permission level for repository metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
    /// Permission level for issues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issues: Option<String>,
    /// Permission level for checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    /// Permission level for statuses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statuses: Option<String>,
}

impl Default for AppPermissions {
    fn default() -> Self {
        Self {
            pull_requests: Some("write".to_string()),
            contents: Some("read".to_string()),
            metadata: Some("read".to_string()),
            issues: Some("read".to_string()),
            checks: None,
            statuses: None,
        }
    }
}

impl AppPermissions {
    /// Create permissions for claudear (PR-focused workflow).
    pub fn for_claudear() -> Self {
        Self::default()
    }

    /// Create minimal permissions (read-only).
    pub fn read_only() -> Self {
        Self {
            pull_requests: Some("read".to_string()),
            contents: Some("read".to_string()),
            metadata: Some("read".to_string()),
            issues: Some("read".to_string()),
            checks: None,
            statuses: None,
        }
    }
}

/// Webhook configuration for the GitHub App.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookAttributes {
    /// The URL where webhook payloads will be sent.
    pub url: String,
    /// Whether the webhook is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

impl HookAttributes {
    /// Create hook attributes for a given base URL.
    pub fn for_base_url(base_url: &str) -> Self {
        Self {
            url: format!("{}/webhook/github", base_url.trim_end_matches('/')),
            active: Some(true),
        }
    }
}

/// GitHub App manifest for the manifest flow.
///
/// This is the JSON structure that GitHub expects when creating an App
/// via the manifest flow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppManifest {
    /// The name of the GitHub App.
    pub name: String,
    /// The homepage URL of the GitHub App.
    pub url: String,
    /// Callback URLs for OAuth authorization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_urls: Option<Vec<String>>,
    /// URL to redirect users after installation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup_url: Option<String>,
    /// URL where GitHub will redirect after App creation.
    pub redirect_url: String,
    /// Webhook configuration.
    pub hook_attributes: HookAttributes,
    /// Whether the App is public (can be installed by anyone).
    pub public: bool,
    /// Default permissions requested during installation.
    pub default_permissions: AppPermissions,
    /// Events the App subscribes to.
    pub default_events: Vec<String>,
}

impl AppManifest {
    /// Generate a manifest for claudear.
    ///
    /// # Arguments
    /// * `base_url` - The public base URL of the claudear instance (e.g., "https://example.com:3100")
    /// * `app_name` - Optional custom name for the App (defaults to "claudear")
    pub fn generate(base_url: &str, app_name: Option<&str>) -> Self {
        let base_url = base_url.trim_end_matches('/');
        let name = app_name.unwrap_or("claudear").to_string();

        Self {
            name,
            url: base_url.to_string(),
            callback_urls: Some(vec![format!("{}/github/callback", base_url)]),
            setup_url: Some(format!("{}/github/installed", base_url)),
            redirect_url: format!("{}/github/callback", base_url),
            hook_attributes: HookAttributes::for_base_url(base_url),
            public: false,
            default_permissions: AppPermissions::for_claudear(),
            default_events: Self::default_events(),
        }
    }

    /// Default events for claudear to subscribe to.
    pub fn default_events() -> Vec<String> {
        vec![
            "pull_request".to_string(),
            "pull_request_review".to_string(),
            "pull_request_review_comment".to_string(),
            "issue_comment".to_string(),
        ]
    }

    /// Serialize the manifest to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Serialize the manifest to pretty-printed JSON.
    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Generate the URL for the GitHub manifest flow.
    ///
    /// # Arguments
    /// * `state` - CSRF state token to include in the redirect
    pub fn github_manifest_url(&self, state: &str) -> Result<String, serde_json::Error> {
        let manifest_json = self.to_json()?;
        let encoded_manifest = urlencoding::encode(&manifest_json);
        let encoded_state = urlencoding::encode(state);

        Ok(format!(
            "https://github.com/settings/apps/new?manifest={}&state={}",
            encoded_manifest, encoded_state
        ))
    }

    /// Generate the URL for creating an org-level App.
    ///
    /// # Arguments
    /// * `org` - The GitHub organization slug
    /// * `state` - CSRF state token to include in the redirect
    pub fn github_org_manifest_url(
        &self,
        org: &str,
        state: &str,
    ) -> Result<String, serde_json::Error> {
        let manifest_json = self.to_json()?;
        let encoded_manifest = urlencoding::encode(&manifest_json);
        let encoded_state = urlencoding::encode(state);

        Ok(format!(
            "https://github.com/organizations/{}/settings/apps/new?manifest={}&state={}",
            org, encoded_manifest, encoded_state
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_permissions_default() {
        let perms = AppPermissions::default();
        assert_eq!(perms.pull_requests, Some("write".to_string()));
        assert_eq!(perms.contents, Some("read".to_string()));
        assert_eq!(perms.metadata, Some("read".to_string()));
        assert_eq!(perms.issues, Some("read".to_string()));
    }

    #[test]
    fn test_app_permissions_read_only() {
        let perms = AppPermissions::read_only();
        assert_eq!(perms.pull_requests, Some("read".to_string()));
        assert_eq!(perms.contents, Some("read".to_string()));
    }

    #[test]
    fn test_hook_attributes_for_base_url() {
        let attrs = HookAttributes::for_base_url("https://example.com:3100");
        assert_eq!(attrs.url, "https://example.com:3100/webhook/github");
        assert_eq!(attrs.active, Some(true));

        // Should handle trailing slash
        let attrs = HookAttributes::for_base_url("https://example.com:3100/");
        assert_eq!(attrs.url, "https://example.com:3100/webhook/github");
    }

    #[test]
    fn test_app_manifest_generate() {
        let manifest = AppManifest::generate("https://example.com:3100", None);

        assert_eq!(manifest.name, "claudear");
        assert_eq!(manifest.url, "https://example.com:3100");
        assert_eq!(
            manifest.redirect_url,
            "https://example.com:3100/github/callback"
        );
        assert_eq!(
            manifest.hook_attributes.url,
            "https://example.com:3100/webhook/github"
        );
        assert!(!manifest.public);
        assert_eq!(manifest.default_events.len(), 4);
        assert!(manifest
            .default_events
            .contains(&"pull_request".to_string()));
    }

    #[test]
    fn test_app_manifest_generate_custom_name() {
        let manifest = AppManifest::generate("https://example.com", Some("my-bot"));
        assert_eq!(manifest.name, "my-bot");
    }

    #[test]
    fn test_app_manifest_generate_trailing_slash() {
        let manifest = AppManifest::generate("https://example.com:3100/", None);

        // Should normalize URLs without double slashes
        assert_eq!(manifest.url, "https://example.com:3100");
        assert_eq!(
            manifest.redirect_url,
            "https://example.com:3100/github/callback"
        );
    }

    #[test]
    fn test_app_manifest_to_json() {
        let manifest = AppManifest::generate("https://example.com", None);
        let json = manifest.to_json().unwrap();

        // Should be valid JSON
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["name"], "claudear");
        assert_eq!(parsed["url"], "https://example.com");
    }

    #[test]
    fn test_app_manifest_scm_url() {
        let manifest = AppManifest::generate("https://example.com", None);
        let url = manifest.github_manifest_url("test_state").unwrap();

        assert!(url.starts_with("https://github.com/settings/apps/new?manifest="));
        assert!(url.contains("state=test_state"));
    }

    #[test]
    fn test_app_manifest_github_org_url() {
        let manifest = AppManifest::generate("https://example.com", None);
        let url = manifest
            .github_org_manifest_url("my-org", "test_state")
            .unwrap();

        assert!(
            url.starts_with("https://github.com/organizations/my-org/settings/apps/new?manifest=")
        );
        assert!(url.contains("state=test_state"));
    }

    #[test]
    fn test_default_events() {
        let events = AppManifest::default_events();
        assert!(events.contains(&"pull_request".to_string()));
        assert!(events.contains(&"pull_request_review".to_string()));
        assert!(events.contains(&"pull_request_review_comment".to_string()));
        assert!(events.contains(&"issue_comment".to_string()));
    }

    #[test]
    fn test_manifest_serialization_roundtrip() {
        let manifest = AppManifest::generate("https://example.com:3100", Some("test-app"));
        let json = manifest.to_json().unwrap();
        let deserialized: AppManifest = serde_json::from_str(&json).unwrap();

        assert_eq!(manifest.name, deserialized.name);
        assert_eq!(manifest.url, deserialized.url);
        assert_eq!(manifest.redirect_url, deserialized.redirect_url);
        assert_eq!(manifest.public, deserialized.public);
    }

    #[test]
    fn test_permissions_serialization_skips_none() {
        let perms = AppPermissions {
            pull_requests: Some("write".to_string()),
            contents: None,
            metadata: None,
            issues: None,
            checks: None,
            statuses: None,
        };

        let json = serde_json::to_string(&perms).unwrap();
        // Should only contain pull_requests, not the None fields
        assert!(json.contains("pull_requests"));
        assert!(!json.contains("contents"));
        assert!(!json.contains("checks"));
    }
}

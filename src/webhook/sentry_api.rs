//! Sentry API client for webhook management.

use crate::config::SentryConfig;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Client for Sentry REST API webhook operations.
pub struct SentryApiClient {
    auth_token: String,
    org_slug: String,
    client: reqwest::Client,
}

/// Result of a webhook registration.
#[derive(Debug, Clone)]
pub struct SentryWebhookRegistration {
    /// The webhook/hook ID.
    pub id: String,
    /// The webhook URL.
    pub url: String,
    /// The signing secret for HMAC verification.
    pub secret: String,
    /// The events subscribed to.
    pub events: Vec<String>,
    /// The project slug this hook is for.
    pub project_slug: String,
}

#[derive(Debug, Serialize)]
struct CreateHookRequest {
    url: String,
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HookResponse {
    id: String,
    url: String,
    secret: String,
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct HookListItem {
    id: String,
    url: String,
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SentryErrorResponse {
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ProjectResponse {
    slug: String,
}

impl SentryApiClient {
    const BASE_URL: &'static str = "https://sentry.io/api/0";

    /// Create a new Sentry API client.
    pub fn new(auth_token: String, org_slug: String) -> Self {
        Self {
            auth_token,
            org_slug,
            client: reqwest::Client::new(),
        }
    }

    /// Create from a SentryConfig.
    pub fn from_config(config: &SentryConfig) -> Self {
        Self::new(config.auth_token.clone(), config.org_slug.clone())
    }

    /// Get the organization slug.
    pub fn org_slug(&self) -> &str {
        &self.org_slug
    }

    /// List all projects in the organization.
    pub async fn list_projects(&self) -> Result<Vec<String>> {
        let url = format!(
            "{}/organizations/{}/projects/",
            Self::BASE_URL,
            self.org_slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Sentry API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Sentry API returned status {}: {}",
                status, body
            )));
        }

        let projects: Vec<ProjectResponse> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Sentry API response: {}", e)))?;

        Ok(projects.into_iter().map(|p| p.slug).collect())
    }

    /// Register a new webhook (service hook) for a project.
    ///
    /// # Arguments
    /// * `project_slug` - The project slug to register the hook for
    /// * `url` - The URL where Sentry will send webhook events
    /// * `events` - List of events to subscribe to (e.g., ["event.created", "event.alert"])
    ///
    /// # Returns
    /// The webhook registration result including the signing secret.
    pub async fn create_webhook(
        &self,
        project_slug: &str,
        url: &str,
        events: &[&str],
    ) -> Result<SentryWebhookRegistration> {
        let api_url = format!(
            "{}/projects/{}/{}/hooks/",
            Self::BASE_URL,
            self.org_slug,
            project_slug
        );

        let request_body = CreateHookRequest {
            url: url.to_string(),
            events: events.iter().map(|s| s.to_string()).collect(),
        };

        let response = self
            .client
            .post(&api_url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Sentry API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());

            // Try to parse the error message
            if let Ok(error_resp) = serde_json::from_str::<SentryErrorResponse>(&body) {
                if let Some(detail) = error_resp.detail {
                    return Err(Error::api(format!(
                        "Sentry API error ({}): {}",
                        status, detail
                    )));
                }
            }

            return Err(Error::api(format!(
                "Sentry API returned status {}: {}",
                status, body
            )));
        }

        let hook: HookResponse = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Sentry API response: {}", e)))?;

        Ok(SentryWebhookRegistration {
            id: hook.id,
            url: hook.url,
            secret: hook.secret,
            events: hook.events,
            project_slug: project_slug.to_string(),
        })
    }

    /// List existing webhooks for a project.
    pub async fn list_webhooks(
        &self,
        project_slug: &str,
    ) -> Result<Vec<(String, String, Vec<String>)>> {
        let url = format!(
            "{}/projects/{}/{}/hooks/",
            Self::BASE_URL,
            self.org_slug,
            project_slug
        );

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Sentry API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Sentry API returned status {}: {}",
                status, body
            )));
        }

        let hooks: Vec<HookListItem> = response
            .json()
            .await
            .map_err(|e| Error::api(format!("Failed to parse Sentry API response: {}", e)))?;

        Ok(hooks.into_iter().map(|h| (h.id, h.url, h.events)).collect())
    }

    /// Check if a webhook with the given URL already exists for a project.
    pub async fn webhook_exists(&self, project_slug: &str, url: &str) -> Result<bool> {
        let webhooks = self.list_webhooks(project_slug).await?;
        Ok(webhooks.iter().any(|(_, wh_url, _)| wh_url == url))
    }

    /// Delete a webhook by ID for a project.
    pub async fn delete_webhook(&self, project_slug: &str, hook_id: &str) -> Result<()> {
        let url = format!(
            "{}/projects/{}/{}/hooks/{}/",
            Self::BASE_URL,
            self.org_slug,
            project_slug,
            hook_id
        );

        let response = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", self.auth_token))
            .send()
            .await
            .map_err(|e| Error::api(format!("Failed to send request to Sentry API: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(Error::api(format!(
                "Sentry API returned status {}: {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Register webhooks for multiple projects.
    ///
    /// If `project_slugs` is empty, attempts to register for all projects in the org.
    /// Returns a list of registration results (one per project).
    pub async fn create_webhooks_for_projects(
        &self,
        project_slugs: &[String],
        url: &str,
        events: &[&str],
    ) -> Result<Vec<SentryWebhookRegistration>> {
        let slugs = if project_slugs.is_empty() {
            self.list_projects().await?
        } else {
            project_slugs.to_vec()
        };

        let mut registrations = Vec::new();
        let mut errors = Vec::new();

        for slug in &slugs {
            match self.create_webhook(slug, url, events).await {
                Ok(reg) => registrations.push(reg),
                Err(e) => {
                    // Log but continue with other projects
                    tracing::warn!("Failed to create webhook for project {}: {}", slug, e);
                    errors.push(format!("{}: {}", slug, e));
                }
            }
        }

        if registrations.is_empty() && !errors.is_empty() {
            return Err(Error::api(format!(
                "Failed to create webhooks for any project: {}",
                errors.join("; ")
            )));
        }

        Ok(registrations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TopIssuesPeriod;

    #[test]
    fn test_sentry_api_client_new() {
        let client = SentryApiClient::new("token".to_string(), "my-org".to_string());
        assert_eq!(client.auth_token, "token");
        assert_eq!(client.org_slug, "my-org");
    }

    #[test]
    fn test_sentry_api_client_from_config() {
        let config = SentryConfig {
            enabled: true,
            auth_token: "sentry_token".to_string(),
            org_slug: "test-org".to_string(),
            project_slugs: vec!["proj1".to_string()],
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::OneDay,
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            ..Default::default()
        };
        let client = SentryApiClient::from_config(&config);
        assert_eq!(client.auth_token, "sentry_token");
        assert_eq!(client.org_slug, "test-org");
    }

    #[test]
    fn test_sentry_api_client_org_slug() {
        let client = SentryApiClient::new("token".to_string(), "my-org".to_string());
        assert_eq!(client.org_slug(), "my-org");
    }

    #[test]
    fn test_sentry_webhook_registration_fields() {
        let registration = SentryWebhookRegistration {
            id: "hook_123".to_string(),
            url: "https://example.com/webhook".to_string(),
            secret: "secret_abc".to_string(),
            events: vec!["event.created".to_string()],
            project_slug: "my-project".to_string(),
        };
        assert_eq!(registration.id, "hook_123");
        assert_eq!(registration.url, "https://example.com/webhook");
        assert_eq!(registration.secret, "secret_abc");
        assert_eq!(registration.events, vec!["event.created"]);
        assert_eq!(registration.project_slug, "my-project");
    }

    #[test]
    fn test_create_hook_request_serialization() {
        let request = CreateHookRequest {
            url: "https://example.com/hook".to_string(),
            events: vec!["event.created".to_string(), "event.alert".to_string()],
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("url"));
        assert!(json.contains("events"));
        assert!(json.contains("event.created"));
    }

    #[test]
    fn test_hook_response_deserialization() {
        let json = r#"{
            "id": "123",
            "url": "https://example.com/hook",
            "secret": "abc123",
            "events": ["event.created"],
            "status": "active",
            "dateCreated": "2024-01-01T00:00:00Z"
        }"#;
        let response: HookResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id, "123");
        assert_eq!(response.url, "https://example.com/hook");
        assert_eq!(response.secret, "abc123");
    }

    #[test]
    fn test_hook_list_item_deserialization() {
        let json = r#"[
            {
                "id": "1",
                "url": "https://example.com/hook1",
                "events": ["event.created"],
                "status": "active"
            },
            {
                "id": "2",
                "url": "https://example.com/hook2",
                "events": ["event.alert"],
                "status": "disabled"
            }
        ]"#;
        let hooks: Vec<HookListItem> = serde_json::from_str(json).unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks[0].id, "1");
        assert_eq!(hooks[1].url, "https://example.com/hook2");
    }
}

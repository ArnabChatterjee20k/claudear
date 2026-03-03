//! GitLab webhook handlers for MR review events and issue events.
//!
//! Two handlers are provided:
//!
//! - `GitLabMrWebhookHandler`: Processes `Merge Request Hook` and `Note Hook` events
//!   to trigger the ReviewWatcher for real-time MR review processing.
//!
//! - `GitLabIssueWebhookHandler`: Implements the `WebhookHandler` trait for processing
//!   `Issue Hook` events as issue sources.

use super::WebhookHandler;
use crate::scm::ReviewWatcher;
use async_trait::async_trait;
use claudear_config::config::GitLabConfig;
use claudear_core::error::Result;
use claudear_core::secret::OptionalSecretExt;
use claudear_core::types::{Issue, IssueStatus, MatchResult};
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// Verify a GitLab webhook token using constant-time comparison.
fn verify_gitlab_token(secret: Option<&str>, headers: &HashMap<String, String>) -> bool {
    let secret = match secret {
        Some(s) => s,
        None => {
            tracing::error!(
                source = "gitlab",
                "No webhook secret configured - rejecting request for security"
            );
            return false;
        }
    };

    let token = match headers.get("x-gitlab-token") {
        Some(t) => t,
        None => {
            tracing::warn!(source = "gitlab", "Missing x-gitlab-token header");
            return false;
        }
    };

    token.as_bytes().ct_eq(secret.as_bytes()).into()
}

/// GitLab webhook handler for MR review events.
///
/// Unlike other webhook handlers, this one does not implement the `WebhookHandler` trait
/// because it doesn't create issues -- it processes MR review events and integrates
/// directly with the ReviewWatcher.
pub struct GitLabMrWebhookHandler {
    review_watcher: Option<Arc<ReviewWatcher>>,
    secret: Option<String>,
    gitlab_base_url: String,
}

impl GitLabMrWebhookHandler {
    /// Create a new GitLab MR webhook handler.
    pub fn new(
        review_watcher: Option<Arc<ReviewWatcher>>,
        secret: Option<String>,
        gitlab_base_url: String,
    ) -> Self {
        Self {
            review_watcher,
            secret,
            gitlab_base_url,
        }
    }

    /// Get the source name.
    pub fn source_name(&self) -> &str {
        "gitlab"
    }

    /// Verify the webhook token using constant-time comparison.
    ///
    /// GitLab uses `X-Gitlab-Token` header with a plain token value (not HMAC).
    pub fn verify_signature(&self, _payload: &[u8], headers: &HashMap<String, String>) -> bool {
        // self.secret is Option<String>, which has as_deref()
        verify_gitlab_token(self.secret.as_deref(), headers)
    }

    /// Check if this handler is enabled (has webhook secret and review watcher).
    pub fn is_enabled(&self) -> bool {
        self.secret.is_some() && self.review_watcher.is_some()
    }

    /// Get the event type from headers.
    /// Headers are expected to be lowercased by the webhook server.
    pub fn get_event_type(headers: &HashMap<String, String>) -> Option<&str> {
        headers.get("x-gitlab-event").map(|s| s.as_str())
    }

    /// Process a webhook payload.
    ///
    /// SECURITY: This method verifies the webhook token before processing.
    ///
    /// Returns Ok(true) if the event was processed, Ok(false) if it was ignored,
    /// or an error if processing failed.
    pub async fn process_webhook(
        &self,
        raw_payload: &[u8],
        payload: &serde_json::Value,
        headers: &HashMap<String, String>,
    ) -> Result<bool> {
        // Verify token before processing any webhook data
        if !self.verify_signature(raw_payload, headers) {
            tracing::warn!(
                source = "gitlab",
                "Webhook token verification failed - rejecting request"
            );
            return Err(claudear_core::error::Error::Webhook(
                "Invalid webhook token".to_string(),
            ));
        }

        let event_type = match Self::get_event_type(headers) {
            Some(t) => t,
            None => {
                tracing::warn!(source = "gitlab", "Missing x-gitlab-event header");
                return Ok(false);
            }
        };

        match event_type {
            "Merge Request Hook" => self.handle_merge_request(payload).await,
            "Note Hook" => self.handle_note(payload).await,
            _ => {
                tracing::debug!(
                    source = "gitlab",
                    event_type = %event_type,
                    "Ignoring non-MR/note event"
                );
                Ok(false)
            }
        }
    }

    /// Handle a Merge Request Hook event.
    async fn handle_merge_request(&self, payload: &serde_json::Value) -> Result<bool> {
        let action = payload
            .get("object_attributes")
            .and_then(|a| a.get("action"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Only process update/approved/unapproved actions
        if !matches!(action, "update" | "approved" | "unapproved") {
            tracing::debug!(
                source = "gitlab",
                action = %action,
                "Ignoring non-review MR action"
            );
            return Ok(false);
        }

        let mr_url = self.extract_mr_url(payload);

        let review_watcher = match &self.review_watcher {
            Some(rw) => rw,
            None => {
                tracing::debug!(
                    source = "gitlab",
                    mr_url = %mr_url,
                    "ReviewWatcher not available, ignoring event"
                );
                return Ok(false);
            }
        };

        // Check if we're watching this MR
        let state = match review_watcher.get_state(&mr_url) {
            Some(s) if s.is_active => s,
            _ => {
                tracing::debug!(
                    source = "gitlab",
                    mr_url = %mr_url,
                    "MR not being watched, ignoring event"
                );
                return Ok(false);
            }
        };

        tracing::info!(
            source = "gitlab",
            mr_url = %mr_url,
            action = %action,
            issue_id = %state.issue_id,
            "Received MR event via webhook"
        );

        let processed_events = review_watcher.check_for_pr(&mr_url).await?;
        tracing::info!(
            source = "gitlab",
            mr_url = %mr_url,
            events = processed_events.len(),
            "Processed MR webhook through ReviewWatcher"
        );

        Ok(true)
    }

    /// Handle a Note Hook event (comments on MRs).
    async fn handle_note(&self, payload: &serde_json::Value) -> Result<bool> {
        // Only process notes on merge requests
        let noteable_type = payload
            .get("object_attributes")
            .and_then(|a| a.get("noteable_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if noteable_type != "MergeRequest" {
            tracing::debug!(
                source = "gitlab",
                noteable_type = %noteable_type,
                "Ignoring note on non-MR object"
            );
            return Ok(false);
        }

        let mr_url = self.extract_mr_url(payload);

        let review_watcher = match &self.review_watcher {
            Some(rw) => rw,
            None => {
                tracing::debug!(
                    source = "gitlab",
                    mr_url = %mr_url,
                    "ReviewWatcher not available, ignoring note event"
                );
                return Ok(false);
            }
        };

        // Check if we're watching this MR
        let state = match review_watcher.get_state(&mr_url) {
            Some(s) if s.is_active => s,
            _ => {
                tracing::debug!(
                    source = "gitlab",
                    mr_url = %mr_url,
                    "MR not being watched, ignoring note"
                );
                return Ok(false);
            }
        };

        let author = payload
            .get("object_attributes")
            .and_then(|a| a.get("author_id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        tracing::info!(
            source = "gitlab",
            mr_url = %mr_url,
            author_id = author,
            issue_id = %state.issue_id,
            "Received MR note via webhook"
        );

        let processed_events = review_watcher.check_for_pr(&mr_url).await?;
        tracing::info!(
            source = "gitlab",
            mr_url = %mr_url,
            events = processed_events.len(),
            "Processed note webhook through ReviewWatcher"
        );

        Ok(true)
    }

    /// Extract the MR URL from a webhook payload.
    ///
    /// For MR hooks, the URL is at `object_attributes.url`.
    /// For note hooks on MRs, it's at `merge_request.url`.
    fn extract_mr_url(&self, payload: &serde_json::Value) -> String {
        // Try object_attributes.url first (MR hook)
        if let Some(url) = payload
            .get("object_attributes")
            .and_then(|a| a.get("url"))
            .and_then(|v| v.as_str())
        {
            return url.to_string();
        }

        // Try merge_request.url (note hook on MR)
        if let Some(url) = payload
            .get("merge_request")
            .and_then(|mr| mr.get("url"))
            .and_then(|v| v.as_str())
        {
            return url.to_string();
        }

        // Fallback: construct from project + MR IID
        let project_url = payload
            .get("project")
            .and_then(|p| p.get("web_url"))
            .and_then(|v| v.as_str())
            .unwrap_or(&self.gitlab_base_url);

        let mr_iid = payload
            .get("object_attributes")
            .and_then(|a| a.get("iid"))
            .or_else(|| payload.get("merge_request").and_then(|mr| mr.get("iid")))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        format!("{}/-/merge_requests/{}", project_url, mr_iid)
    }
}

/// GitLab webhook handler for issue events.
///
/// Implements the `WebhookHandler` trait to process `Issue Hook` events
/// and convert them into the unified Issue type.
pub struct GitLabIssueWebhookHandler {
    config: GitLabConfig,
}

impl GitLabIssueWebhookHandler {
    /// Create a new GitLab issue webhook handler.
    pub fn new(config: GitLabConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WebhookHandler for GitLabIssueWebhookHandler {
    fn source_name(&self) -> &str {
        "gitlab"
    }

    fn verify_signature(&self, _payload: &[u8], headers: &HashMap<String, String>) -> bool {
        verify_gitlab_token(self.config.webhook_secret.expose_as_deref(), headers)
    }

    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>> {
        // Only process Issue Hook events
        let event_type = payload
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if event_type != "issue" {
            return Ok(None);
        }

        let attrs = match payload.get("object_attributes") {
            Some(a) => a,
            None => return Ok(None),
        };

        let iid = attrs.get("iid").and_then(|v| v.as_i64()).unwrap_or(0);

        let title = attrs
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled")
            .to_string();

        let description = attrs
            .get("description")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let state = attrs
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("opened")
            .to_string();

        let web_url = attrs
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        // Extract labels from the top-level "labels" array
        let labels: Vec<String> = payload
            .get("labels")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|l| {
                        l.get("title")
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Extract project path
        let project_path = payload
            .get("project")
            .and_then(|p| p.get("path_with_namespace"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let issue_id = format!("{}:{}", project_path, iid);
        let short_id = issue_id.clone();

        let mut issue = Issue::new(&issue_id, &short_id, &title, &web_url, "gitlab");
        issue.description = description;

        issue.status = match state.as_str() {
            "closed" => IssueStatus::Resolved,
            "opened" => IssueStatus::Open,
            _ => IssueStatus::Open,
        };

        issue.set_metadata("state", &state);
        issue.set_metadata("labels", labels.join(", "));
        issue.set_metadata("project_path", &project_path);
        issue.set_metadata("iid", iid);

        Ok(Some(issue))
    }

    fn matches_criteria(&self, issue: &Issue) -> MatchResult {
        crate::source::gitlab_matches_criteria(&self.config, issue)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        Ok(crate::source::format_gitlab_context(issue))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> GitLabConfig {
        GitLabConfig::test_default()
    }

    #[test]
    fn test_mr_handler_source_name() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("secret".to_string()),
            "https://gitlab.com".to_string(),
        );
        assert_eq!(handler.source_name(), "gitlab");
    }

    #[test]
    fn test_mr_handler_verify_signature_valid() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("my_secret_token".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "my_secret_token".to_string());

        assert!(handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_mr_handler_verify_signature_invalid() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("my_secret_token".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "wrong_token".to_string());

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_mr_handler_verify_signature_missing_header() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("my_secret_token".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = b"test payload";
        let headers = HashMap::new();

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_mr_handler_verify_signature_no_secret() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "some_token".to_string());

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_mr_handler_is_enabled() {
        // No secret, no watcher
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());
        assert!(!handler.is_enabled());

        // Has secret, no watcher
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("secret".to_string()),
            "https://gitlab.com".to_string(),
        );
        assert!(!handler.is_enabled());
    }

    #[test]
    fn test_get_event_type() {
        let mut headers = HashMap::new();
        assert!(GitLabMrWebhookHandler::get_event_type(&headers).is_none());

        headers.insert(
            "x-gitlab-event".to_string(),
            "Merge Request Hook".to_string(),
        );
        assert_eq!(
            GitLabMrWebhookHandler::get_event_type(&headers),
            Some("Merge Request Hook")
        );

        headers.clear();
        headers.insert("x-gitlab-event".to_string(), "Note Hook".to_string());
        assert_eq!(
            GitLabMrWebhookHandler::get_event_type(&headers),
            Some("Note Hook")
        );
    }

    #[test]
    fn test_extract_mr_url_from_object_attributes() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = serde_json::json!({
            "object_attributes": {
                "url": "https://gitlab.com/mygroup/myproject/-/merge_requests/1"
            }
        });

        assert_eq!(
            handler.extract_mr_url(&payload),
            "https://gitlab.com/mygroup/myproject/-/merge_requests/1"
        );
    }

    #[test]
    fn test_extract_mr_url_from_merge_request() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = serde_json::json!({
            "object_attributes": {
                "noteable_type": "MergeRequest"
            },
            "merge_request": {
                "url": "https://gitlab.com/mygroup/myproject/-/merge_requests/5"
            }
        });

        assert_eq!(
            handler.extract_mr_url(&payload),
            "https://gitlab.com/mygroup/myproject/-/merge_requests/5"
        );
    }

    #[test]
    fn test_extract_mr_url_fallback() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = serde_json::json!({
            "object_attributes": {
                "iid": 42
            },
            "project": {
                "web_url": "https://gitlab.com/mygroup/myproject"
            }
        });

        assert_eq!(
            handler.extract_mr_url(&payload),
            "https://gitlab.com/mygroup/myproject/-/merge_requests/42"
        );
    }

    #[test]
    fn test_issue_handler_source_name() {
        let handler = GitLabIssueWebhookHandler::new(test_config());
        assert_eq!(handler.source_name(), "gitlab");
    }

    #[test]
    fn test_issue_handler_verify_signature_valid() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "test_secret".to_string());

        assert!(handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_issue_handler_verify_signature_invalid() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "wrong_secret".to_string());

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_issue_handler_verify_signature_no_secret() {
        let mut config = test_config();
        config.webhook_secret = None;
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "token".to_string());

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[tokio::test]
    async fn test_issue_handler_parse_payload_issue() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let payload = serde_json::json!({
            "event_type": "issue",
            "object_attributes": {
                "iid": 42,
                "title": "Fix the bug",
                "description": "Something is broken",
                "state": "opened",
                "url": "https://gitlab.com/mygroup/myproject/-/issues/42"
            },
            "labels": [
                {"title": "auto-implement"},
                {"title": "bug"}
            ],
            "project": {
                "path_with_namespace": "mygroup/myproject"
            }
        });

        let issue = handler.parse_payload(&payload).await.unwrap().unwrap();
        assert_eq!(issue.id, "mygroup/myproject:42");
        assert_eq!(issue.short_id, "mygroup/myproject:42");
        assert_eq!(issue.title, "Fix the bug");
        assert_eq!(issue.description, Some("Something is broken".to_string()));
        assert_eq!(issue.source, "gitlab");
        assert_eq!(issue.status, IssueStatus::Open);
        assert_eq!(
            issue.get_metadata::<String>("state"),
            Some("opened".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("labels"),
            Some("auto-implement, bug".to_string())
        );
    }

    #[tokio::test]
    async fn test_issue_handler_parse_payload_non_issue() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let payload = serde_json::json!({
            "event_type": "merge_request",
            "object_attributes": {
                "iid": 1,
                "title": "MR title"
            }
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_issue_handler_parse_payload_closed() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let payload = serde_json::json!({
            "event_type": "issue",
            "object_attributes": {
                "iid": 1,
                "title": "Done issue",
                "state": "closed",
                "url": "https://gitlab.com/mygroup/proj/-/issues/1"
            },
            "labels": [],
            "project": {
                "path_with_namespace": "mygroup/proj"
            }
        });

        let issue = handler.parse_payload(&payload).await.unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Resolved);
    }

    #[test]
    fn test_issue_handler_matches_criteria_match() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let mut issue = Issue::new(
            "mygroup/proj:1",
            "mygroup/proj:1",
            "Test",
            "https://gitlab.com/mygroup/proj/-/issues/1",
            "gitlab",
        );
        issue.set_metadata("state", "opened");
        issue.set_metadata("labels", "auto-implement");

        let result = handler.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_issue_handler_matches_criteria_wrong_state() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let mut issue = Issue::new(
            "mygroup/proj:1",
            "mygroup/proj:1",
            "Test",
            "https://gitlab.com/mygroup/proj/-/issues/1",
            "gitlab",
        );
        issue.set_metadata("state", "closed");
        issue.set_metadata("labels", "auto-implement");

        let result = handler.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(result.reason.contains("not in trigger_states"));
    }

    #[test]
    fn test_issue_handler_matches_criteria_wrong_labels() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let mut issue = Issue::new(
            "mygroup/proj:1",
            "mygroup/proj:1",
            "Test",
            "https://gitlab.com/mygroup/proj/-/issues/1",
            "gitlab",
        );
        issue.set_metadata("state", "opened");
        issue.set_metadata("labels", "unrelated");

        let result = handler.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(result.reason.contains("No matching trigger labels"));
    }

    #[tokio::test]
    async fn test_issue_handler_build_context() {
        let handler = GitLabIssueWebhookHandler::new(test_config());

        let mut issue = Issue::new(
            "mygroup/proj:42",
            "mygroup/proj:42",
            "Fix the bug",
            "https://gitlab.com/mygroup/proj/-/issues/42",
            "gitlab",
        );
        issue.description = Some("Detailed description".to_string());
        issue.set_metadata("state", "opened");
        issue.set_metadata("labels", "auto-implement, bug");
        issue.set_metadata("project_path", "mygroup/proj");

        let context = handler.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("# GitLab Issue: mygroup/proj:42"));
        assert!(context.contains("**Title:** Fix the bug"));
        assert!(context.contains("**State:** opened"));
        assert!(context.contains("**Labels:** auto-implement, bug"));
        assert!(context.contains("**Project:** mygroup/proj"));
        assert!(context.contains("## Description"));
        assert!(context.contains("Detailed description"));
    }

    #[tokio::test]
    async fn test_process_webhook_invalid_token_returns_error() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("correct-token".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = serde_json::json!({});
        let raw = serde_json::to_vec(&payload).unwrap();
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "wrong-token".to_string());

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid webhook token"));
    }

    #[tokio::test]
    async fn test_process_webhook_missing_event_type_returns_false() {
        // No secret => verification passes
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("test-secret".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = serde_json::json!({});
        let raw = serde_json::to_vec(&payload).unwrap();
        // Include token but no x-gitlab-event header
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "test-secret".to_string());

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_process_webhook_unknown_event_type_returns_false() {
        let handler = GitLabMrWebhookHandler::new(
            None,
            Some("test-secret".to_string()),
            "https://gitlab.com".to_string(),
        );

        let payload = serde_json::json!({});
        let raw = serde_json::to_vec(&payload).unwrap();
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "test-secret".to_string());
        headers.insert("x-gitlab-event".to_string(), "Push Hook".to_string());

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    fn make_test_handler() -> GitLabMrWebhookHandler {
        GitLabMrWebhookHandler::new(
            None,
            Some("test-secret".to_string()),
            "https://gitlab.com".to_string(),
        )
    }

    fn authed_headers(event: &str) -> HashMap<String, String> {
        let mut headers = HashMap::new();
        headers.insert("x-gitlab-token".to_string(), "test-secret".to_string());
        headers.insert("x-gitlab-event".to_string(), event.to_string());
        headers
    }

    #[tokio::test]
    async fn test_handle_merge_request_ignores_non_review_action() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "action": "open",
                "url": "https://gitlab.com/test/project/-/merge_requests/1"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Merge Request Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_merge_request_no_review_watcher_returns_false() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "action": "update",
                "url": "https://gitlab.com/test/project/-/merge_requests/1"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Merge Request Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_merge_request_approved_action_no_watcher() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "action": "approved",
                "url": "https://gitlab.com/test/project/-/merge_requests/5"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Merge Request Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_merge_request_unapproved_action_no_watcher() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "action": "unapproved",
                "url": "https://gitlab.com/test/project/-/merge_requests/5"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Merge Request Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_note_non_mr_returns_false() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "noteable_type": "Issue",
                "note": "Some comment on an issue"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Note Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_note_mr_no_review_watcher_returns_false() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "noteable_type": "MergeRequest",
                "note": "Looks good",
                "author_id": 42
            },
            "merge_request": {
                "url": "https://gitlab.com/test/project/-/merge_requests/1"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Note Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[tokio::test]
    async fn test_handle_note_missing_noteable_type_returns_false() {
        let handler = make_test_handler();
        let payload = serde_json::json!({
            "object_attributes": {
                "note": "A comment with no noteable_type"
            }
        });
        let raw = serde_json::to_vec(&payload).unwrap();
        let headers = authed_headers("Note Hook");

        let result = handler.process_webhook(&raw, &payload, &headers).await;
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn test_extract_mr_url_from_note_payload() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = serde_json::json!({
            "merge_request": {
                "url": "https://gitlab.com/group/project/-/merge_requests/99"
            }
        });

        let url = handler.extract_mr_url(&payload);
        assert_eq!(url, "https://gitlab.com/group/project/-/merge_requests/99");
    }

    #[test]
    fn test_extract_mr_url_fallback_to_project_and_iid() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        let payload = serde_json::json!({
            "project": {
                "web_url": "https://gitlab.com/myorg/myproject"
            },
            "object_attributes": {
                "iid": 7
            }
        });

        let url = handler.extract_mr_url(&payload);
        assert_eq!(url, "https://gitlab.com/myorg/myproject/-/merge_requests/7");
    }

    #[test]
    fn test_extract_mr_url_fallback_to_merge_request_iid() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        // No object_attributes.iid, but merge_request.iid exists
        let payload = serde_json::json!({
            "project": {
                "web_url": "https://gitlab.com/team/service"
            },
            "merge_request": {
                "iid": 12
            }
        });

        let url = handler.extract_mr_url(&payload);
        assert_eq!(url, "https://gitlab.com/team/service/-/merge_requests/12");
    }

    #[test]
    fn test_extract_mr_url_complete_fallback() {
        let handler = GitLabMrWebhookHandler::new(None, None, "https://gitlab.com".to_string());

        // No URLs, no project, no IID
        let payload = serde_json::json!({});

        let url = handler.extract_mr_url(&payload);
        // Falls back to gitlab_base_url and iid=0
        assert!(url.contains("/-/merge_requests/0"));
    }

    #[tokio::test]
    async fn test_issue_parse_payload_non_issue_event() {
        let config = test_config();
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = serde_json::json!({
            "event_type": "merge_request"
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_issue_parse_payload_missing_object_attributes() {
        let config = test_config();
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = serde_json::json!({
            "event_type": "issue"
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_issue_parse_payload_closed_state() {
        let config = test_config();
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = serde_json::json!({
            "event_type": "issue",
            "object_attributes": {
                "iid": 10,
                "title": "Closed issue",
                "description": "Already resolved",
                "state": "closed",
                "url": "https://gitlab.com/group/project/-/issues/10"
            },
            "project": {
                "path_with_namespace": "group/project"
            },
            "labels": []
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_some());
        let issue = result.unwrap();
        assert_eq!(issue.status, claudear_core::types::IssueStatus::Resolved);
        assert_eq!(issue.id, "group/project:10");
    }

    #[tokio::test]
    async fn test_issue_parse_payload_no_labels() {
        let config = test_config();
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = serde_json::json!({
            "event_type": "issue",
            "object_attributes": {
                "iid": 15,
                "title": "No labels issue",
                "state": "opened",
                "url": "https://gitlab.com/group/project/-/issues/15"
            },
            "project": {
                "path_with_namespace": "group/project"
            }
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_some());
        let issue = result.unwrap();
        assert_eq!(issue.title, "No labels issue");
        // labels metadata should be empty string
        let labels = issue.metadata.get("labels").and_then(|v| v.as_str());
        assert_eq!(labels, Some(""));
    }

    #[tokio::test]
    async fn test_issue_parse_payload_with_description() {
        let config = test_config();
        let handler = GitLabIssueWebhookHandler::new(config);

        let payload = serde_json::json!({
            "event_type": "issue",
            "object_attributes": {
                "iid": 20,
                "title": "Detailed bug",
                "description": "Steps to reproduce: ...",
                "state": "opened",
                "url": "https://gitlab.com/group/project/-/issues/20"
            },
            "project": {
                "path_with_namespace": "group/project"
            },
            "labels": [
                {"title": "bug"},
                {"title": "priority::high"}
            ]
        });

        let result = handler.parse_payload(&payload).await.unwrap();
        assert!(result.is_some());
        let issue = result.unwrap();
        assert_eq!(
            issue.description,
            Some("Steps to reproduce: ...".to_string())
        );
        let labels = issue.metadata.get("labels").and_then(|v| v.as_str());
        assert_eq!(labels, Some("bug, priority::high"));
    }
}

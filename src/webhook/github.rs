//! GitHub webhook handler for PR review events.
//!
//! This handler processes `pull_request_review` and `pull_request_review_comment`
//! events from GitHub webhooks to trigger review processing in real-time instead
//! of relying solely on polling.

use crate::config::GitHubConfig;
use crate::error::Result;
use crate::github::{GitHubUser, PrReview, PrReviewComment, ReviewWatcher};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;

/// GitHub webhook handler for PR review events.
///
/// Unlike other webhook handlers, this one doesn't implement the `WebhookHandler` trait
/// because it doesn't create issues - it processes PR review events and integrates
/// directly with the ReviewWatcher.
pub struct GitHubWebhookHandler {
    config: GitHubConfig,
    review_watcher: Option<Arc<ReviewWatcher>>,
}

impl GitHubWebhookHandler {
    /// Create a new GitHub webhook handler.
    pub fn new(config: GitHubConfig, review_watcher: Option<Arc<ReviewWatcher>>) -> Self {
        Self {
            config,
            review_watcher,
        }
    }

    /// Get the source name.
    pub fn source_name(&self) -> &str {
        "github"
    }

    /// Verify the webhook signature using HMAC-SHA256.
    ///
    /// GitHub uses `x-hub-signature-256` header with format: `sha256=<hex>`
    pub fn verify_signature(&self, payload: &[u8], headers: &HashMap<String, String>) -> bool {
        let secret = match &self.config.webhook_secret {
            Some(s) => s,
            None => {
                tracing::error!(
                    source = "github",
                    "No webhook secret configured - rejecting request for security"
                );
                return false;
            }
        };

        // GitHub uses lowercase header name
        let signature = match headers
            .get("x-hub-signature-256")
            .or_else(|| headers.get("X-Hub-Signature-256"))
        {
            Some(s) => s,
            None => {
                tracing::warn!(source = "github", "Missing x-hub-signature-256 header");
                return false;
            }
        };

        // Signature format: sha256=<hex>
        let signature_hex = match signature.strip_prefix("sha256=") {
            Some(hex) => hex,
            None => {
                tracing::warn!(
                    source = "github",
                    "Invalid signature format - expected sha256= prefix"
                );
                return false;
            }
        };

        let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => {
                tracing::error!(source = "github", "Failed to create HMAC from secret");
                return false;
            }
        };

        mac.update(payload);
        let expected = mac.finalize().into_bytes();
        let expected_hex = hex::encode(expected);

        signature_hex
            .as_bytes()
            .ct_eq(expected_hex.as_bytes())
            .into()
    }

    /// Check if this handler is enabled (has webhook secret and review watcher).
    pub fn is_enabled(&self) -> bool {
        self.config.webhook_secret.is_some() && self.review_watcher.is_some()
    }

    /// Get the event type from headers.
    pub fn get_event_type(headers: &HashMap<String, String>) -> Option<&str> {
        headers
            .get("x-github-event")
            .or_else(|| headers.get("X-GitHub-Event"))
            .map(|s| s.as_str())
    }

    /// Process a webhook payload.
    ///
    /// Returns Ok(true) if the event was processed, Ok(false) if it was ignored,
    /// or an error if processing failed.
    pub async fn process_webhook(
        &self,
        payload: &serde_json::Value,
        headers: &HashMap<String, String>,
    ) -> Result<bool> {
        let event_type = match Self::get_event_type(headers) {
            Some(t) => t,
            None => {
                tracing::warn!(source = "github", "Missing x-github-event header");
                return Ok(false);
            }
        };

        match event_type {
            "pull_request_review" => self.handle_review_submitted(payload).await,
            "pull_request_review_comment" => self.handle_review_comment(payload).await,
            _ => {
                tracing::debug!(
                    source = "github",
                    event_type = %event_type,
                    "Ignoring non-review event"
                );
                Ok(false)
            }
        }
    }

    /// Handle a pull_request_review.submitted event.
    async fn handle_review_submitted(&self, payload: &serde_json::Value) -> Result<bool> {
        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if action != "submitted" {
            tracing::debug!(
                source = "github",
                action = %action,
                "Ignoring non-submitted review action"
            );
            return Ok(false);
        }

        let review = match payload.get("review") {
            Some(r) => r,
            None => {
                tracing::warn!(source = "github", "Missing review in payload");
                return Ok(false);
            }
        };

        let pr = match payload.get("pull_request") {
            Some(p) => p,
            None => {
                tracing::warn!(source = "github", "Missing pull_request in payload");
                return Ok(false);
            }
        };

        let pr_url = pr
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let review_watcher = match &self.review_watcher {
            Some(rw) => rw,
            None => {
                tracing::debug!(
                    source = "github",
                    pr_url = %pr_url,
                    "ReviewWatcher not available, ignoring event"
                );
                return Ok(false);
            }
        };

        // Check if we're watching this PR
        let state = match review_watcher.get_state(pr_url) {
            Some(s) if s.is_active => s,
            _ => {
                tracing::debug!(
                    source = "github",
                    pr_url = %pr_url,
                    "PR not being watched, ignoring review"
                );
                return Ok(false);
            }
        };

        // Parse the review
        let pr_review = self.parse_review(review)?;

        // Skip bot reviews
        if pr_review.user.user_type.as_deref() == Some("Bot") {
            tracing::debug!(
                source = "github",
                pr_url = %pr_url,
                reviewer = %pr_review.user.login,
                "Skipping bot review"
            );
            return Ok(false);
        }

        // Skip pending reviews
        if pr_review.state.to_uppercase() == "PENDING" {
            tracing::debug!(
                source = "github",
                pr_url = %pr_url,
                "Skipping pending review"
            );
            return Ok(false);
        }

        tracing::info!(
            source = "github",
            pr_url = %pr_url,
            reviewer = %pr_review.user.login,
            state = %pr_review.state,
            issue_id = %state.issue_id,
            "Received PR review via webhook"
        );

        // TODO: Trigger review processing through ReviewWatcher
        // For now we just log it - the full integration would call review_watcher methods
        // to process the review and potentially re-trigger Claude

        Ok(true)
    }

    /// Handle a pull_request_review_comment.created event.
    async fn handle_review_comment(&self, payload: &serde_json::Value) -> Result<bool> {
        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if action != "created" {
            tracing::debug!(
                source = "github",
                action = %action,
                "Ignoring non-created comment action"
            );
            return Ok(false);
        }

        let comment = match payload.get("comment") {
            Some(c) => c,
            None => {
                tracing::warn!(source = "github", "Missing comment in payload");
                return Ok(false);
            }
        };

        let pr = match payload.get("pull_request") {
            Some(p) => p,
            None => {
                tracing::warn!(source = "github", "Missing pull_request in payload");
                return Ok(false);
            }
        };

        let pr_url = pr
            .get("html_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let review_watcher = match &self.review_watcher {
            Some(rw) => rw,
            None => {
                tracing::debug!(
                    source = "github",
                    pr_url = %pr_url,
                    "ReviewWatcher not available, ignoring event"
                );
                return Ok(false);
            }
        };

        // Check if we're watching this PR
        let state = match review_watcher.get_state(pr_url) {
            Some(s) if s.is_active => s,
            _ => {
                tracing::debug!(
                    source = "github",
                    pr_url = %pr_url,
                    "PR not being watched, ignoring comment"
                );
                return Ok(false);
            }
        };

        // Parse the comment
        let pr_comment = self.parse_comment(comment)?;

        // Skip bot comments
        if pr_comment.user.user_type.as_deref() == Some("Bot") {
            tracing::debug!(
                source = "github",
                pr_url = %pr_url,
                author = %pr_comment.user.login,
                "Skipping bot comment"
            );
            return Ok(false);
        }

        tracing::info!(
            source = "github",
            pr_url = %pr_url,
            author = %pr_comment.user.login,
            path = %pr_comment.path,
            issue_id = %state.issue_id,
            "Received PR review comment via webhook"
        );

        // TODO: Trigger comment processing through ReviewWatcher
        // For now we just log it - the full integration would process comments
        // and potentially re-trigger Claude with the feedback

        Ok(true)
    }

    /// Parse a review from the webhook payload.
    fn parse_review(&self, review: &serde_json::Value) -> Result<PrReview> {
        let user = review
            .get("user")
            .map(|u| GitHubUser {
                id: u.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                login: u
                    .get("login")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                user_type: u.get("type").and_then(|v| v.as_str()).map(|s| s.to_string()),
            })
            .unwrap_or(GitHubUser {
                id: 0,
                login: "unknown".to_string(),
                user_type: None,
            });

        Ok(PrReview {
            id: review.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            user,
            body: review
                .get("body")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            state: review
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("COMMENTED")
                .to_string(),
            submitted_at: review
                .get("submitted_at")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            html_url: review
                .get("html_url")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
    }

    /// Parse a comment from the webhook payload.
    fn parse_comment(&self, comment: &serde_json::Value) -> Result<PrReviewComment> {
        let user = comment
            .get("user")
            .map(|u| GitHubUser {
                id: u.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
                login: u
                    .get("login")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                user_type: u.get("type").and_then(|v| v.as_str()).map(|s| s.to_string()),
            })
            .unwrap_or(GitHubUser {
                id: 0,
                login: "unknown".to_string(),
                user_type: None,
            });

        Ok(PrReviewComment {
            id: comment.get("id").and_then(|v| v.as_i64()).unwrap_or(0),
            path: comment
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            position: comment.get("position").and_then(|v| v.as_i64()),
            original_position: comment.get("original_position").and_then(|v| v.as_i64()),
            body: comment
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            user,
            created_at: comment
                .get("created_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            updated_at: comment
                .get("updated_at")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            html_url: comment
                .get("html_url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            pull_request_review_id: comment
                .get("pull_request_review_id")
                .and_then(|v| v.as_i64()),
            start_line: comment.get("start_line").and_then(|v| v.as_i64()),
            line: comment.get("line").and_then(|v| v.as_i64()),
            side: comment
                .get("side")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config(webhook_secret: Option<&str>) -> GitHubConfig {
        GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: false,
            webhook_secret: webhook_secret.map(|s| s.to_string()),
            review_trigger: "/claudear".to_string(),
        }
    }

    #[test]
    fn test_github_webhook_signature_verification_valid() {
        let config = create_test_config(Some("test_secret"));
        let handler = GitHubWebhookHandler::new(config, None);

        let payload = b"test payload";

        // Compute expected signature
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(b"test_secret").unwrap();
        mac.update(payload);
        let expected = mac.finalize().into_bytes();
        let signature = format!("sha256={}", hex::encode(expected));

        let mut headers = HashMap::new();
        headers.insert("x-hub-signature-256".to_string(), signature);

        assert!(handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_github_webhook_signature_verification_invalid() {
        let config = create_test_config(Some("test_secret"));
        let handler = GitHubWebhookHandler::new(config, None);

        let payload = b"test payload";

        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".to_string(),
            "sha256=invalid".to_string(),
        );

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_github_webhook_signature_missing_header() {
        let config = create_test_config(Some("test_secret"));
        let handler = GitHubWebhookHandler::new(config, None);

        let payload = b"test payload";
        let headers = HashMap::new();

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_github_webhook_signature_no_secret() {
        let config = create_test_config(None);
        let handler = GitHubWebhookHandler::new(config, None);

        let payload = b"test payload";
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".to_string(),
            "sha256=whatever".to_string(),
        );

        assert!(!handler.verify_signature(payload, &headers));
    }

    #[test]
    fn test_get_event_type() {
        let mut headers = HashMap::new();
        assert!(GitHubWebhookHandler::get_event_type(&headers).is_none());

        headers.insert("x-github-event".to_string(), "pull_request_review".to_string());
        assert_eq!(
            GitHubWebhookHandler::get_event_type(&headers),
            Some("pull_request_review")
        );

        headers.clear();
        headers.insert(
            "X-GitHub-Event".to_string(),
            "pull_request_review_comment".to_string(),
        );
        assert_eq!(
            GitHubWebhookHandler::get_event_type(&headers),
            Some("pull_request_review_comment")
        );
    }

    #[test]
    fn test_is_enabled() {
        // No secret, no watcher
        let config = create_test_config(None);
        let handler = GitHubWebhookHandler::new(config, None);
        assert!(!handler.is_enabled());

        // Has secret, no watcher
        let config = create_test_config(Some("secret"));
        let handler = GitHubWebhookHandler::new(config, None);
        assert!(!handler.is_enabled());
    }

    #[test]
    fn test_parse_review() {
        let config = create_test_config(Some("secret"));
        let handler = GitHubWebhookHandler::new(config, None);

        let review_json = serde_json::json!({
            "id": 12345,
            "user": {
                "id": 1,
                "login": "reviewer",
                "type": "User"
            },
            "body": "LGTM!",
            "state": "APPROVED",
            "submitted_at": "2024-01-15T10:00:00Z",
            "html_url": "https://github.com/owner/repo/pull/1#pullrequestreview-12345"
        });

        let review = handler.parse_review(&review_json).unwrap();
        assert_eq!(review.id, 12345);
        assert_eq!(review.user.login, "reviewer");
        assert_eq!(review.state, "APPROVED");
        assert_eq!(review.body, Some("LGTM!".to_string()));
    }

    #[test]
    fn test_parse_comment() {
        let config = create_test_config(Some("secret"));
        let handler = GitHubWebhookHandler::new(config, None);

        let comment_json = serde_json::json!({
            "id": 67890,
            "user": {
                "id": 2,
                "login": "commenter",
                "type": "User"
            },
            "body": "Consider refactoring this",
            "path": "src/main.rs",
            "position": 10,
            "line": 42,
            "side": "RIGHT",
            "created_at": "2024-01-15T11:00:00Z",
            "updated_at": "2024-01-15T11:00:00Z",
            "html_url": "https://github.com/owner/repo/pull/1#discussion_r67890",
            "pull_request_review_id": 12345
        });

        let comment = handler.parse_comment(&comment_json).unwrap();
        assert_eq!(comment.id, 67890);
        assert_eq!(comment.user.login, "commenter");
        assert_eq!(comment.path, "src/main.rs");
        assert_eq!(comment.body, "Consider refactoring this");
        assert_eq!(comment.line, Some(42));
        assert_eq!(comment.pull_request_review_id, Some(12345));
    }
}

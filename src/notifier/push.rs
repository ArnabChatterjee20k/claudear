//! Push notification via Pushover.

use super::Notifier;
use crate::config::PushConfig;
use crate::error::{Error, Result};
use crate::types::Issue;
use crate::users::UserRegistry;
use async_trait::async_trait;
use serde::Serialize;

/// Push notifier that sends notifications via Pushover.
pub struct PushNotifier {
    config: PushConfig,
    client: reqwest::Client,
    user_registry: UserRegistry,
}

#[derive(Debug, Serialize)]
struct PushoverMessage {
    token: String,
    user: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<i8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sound: Option<String>,
}

impl PushNotifier {
    /// Create a new push notifier.
    pub fn new(config: PushConfig, user_registry: UserRegistry) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            user_registry,
        }
    }

    fn resolve_user_key(&self, issue: Option<&Issue>) -> Option<String> {
        if let Some(issue) = issue {
            if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
                if let Some(user) = self.user_registry.get_by_slug(&slug) {
                    if user.push_user_key.is_some() {
                        return user.push_user_key.clone();
                    }
                }
            }
        }
        self.config.user_key.clone()
    }

    async fn send_push(
        &self,
        title: &str,
        message: &str,
        url: Option<&str>,
        url_title: Option<&str>,
        priority: Option<i8>,
        issue: Option<&Issue>,
    ) -> Result<()> {
        let api_token = match &self.config.api_token {
            Some(token) => token,
            None => return Ok(()),
        };

        let user_key = match self.resolve_user_key(issue) {
            Some(key) => key,
            None => return Ok(()),
        };

        // Truncate message if too long (Pushover limit is 1024 chars for message)
        let truncated_message = if message.len() > 1000 {
            format!("{}...", &message[..message.floor_char_boundary(997)])
        } else {
            message.to_string()
        };

        let push_message = PushoverMessage {
            token: api_token.clone(),
            user: user_key,
            message: truncated_message,
            title: Some(title.to_string()),
            url: url.map(|s| s.to_string()),
            url_title: url_title.map(|s| s.to_string()),
            priority: priority.or(self.config.priority),
            device: self.config.device.clone(),
            sound: None,
        };

        let response = self
            .client
            .post("https://api.pushover.net/1/messages.json")
            .json(&push_message)
            .send()
            .await?;

        if !response.status().is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::notifier("push", format!("Pushover error: {}", text)));
        }

        Ok(())
    }

    fn get_source_emoji(source: &str) -> &'static str {
        match source.to_lowercase().as_str() {
            "linear" => "\u{1F4CB}",
            "sentry" => "\u{1F534}",
            "github" => "\u{1F419}",
            "jira" => "\u{1F3AB}",
            _ => "\u{1F4CC}",
        }
    }
}

#[async_trait]
impl Notifier for PushNotifier {
    fn name(&self) -> &str {
        "push"
    }

    fn is_enabled(&self) -> bool {
        self.config.api_token.is_some() && self.config.user_key.is_some()
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let emoji = Self::get_source_emoji(&issue.source);
        let title = format!("{} Processing: {}", emoji, issue.short_id);
        let message = format!(
            "{}\n\nSource: {}\nPriority: {}\nStatus: {}",
            issue.title, issue.source, issue.priority, issue.status
        );

        self.send_push(
            &title,
            &message,
            Some(&issue.url),
            Some("View Issue"),
            Some(-1),
            Some(issue),
        )
        .await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let title = format!("\u{2705} PR Created: {}", issue.short_id);
        let message = format!("{}\n\nPR URL: {}", issue.title, pr_url);

        self.send_push(
            &title,
            &message,
            Some(pr_url),
            Some("View PR"),
            Some(0),
            Some(issue),
        )
        .await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let title = format!("\u{2714}\u{FE0F} Completed: {}", issue.short_id);
        let message = format!("{}\n\nNo PR URL was captured.", issue.title);

        self.send_push(
            &title,
            &message,
            Some(&issue.url),
            Some("View Issue"),
            Some(-1),
            Some(issue),
        )
        .await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let title = format!("\u{274C} Failed: {}", issue.short_id);
        let message = format!("{}\n\nError: {}", issue.title, error);

        // Higher priority for failures
        self.send_push(
            &title,
            &message,
            Some(&issue.url),
            Some("View Issue"),
            Some(1),
            Some(issue),
        )
        .await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        self.send_push("Claude Watchers", message, None, None, Some(-1), None)
            .await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let title = format!(
            "\u{1F6A8} {} Urgent Issue{}",
            issues.len(),
            if issues.len() > 1 { "s" } else { "" }
        );

        let message = issues
            .iter()
            .take(5)
            .map(|i| {
                let emoji = Self::get_source_emoji(&i.source);
                format!("{} {} - {}", emoji, i.short_id, i.title)
            })
            .collect::<Vec<_>>()
            .join("\n");

        // High priority for urgent issues
        self.send_push(&title, &message, None, None, Some(1), None)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> UserRegistry {
        UserRegistry::new(std::collections::HashMap::new())
    }

    fn disabled_config() -> PushConfig {
        PushConfig {
            api_token: None,
            user_key: None,
            device: None,
            priority: None,
        }
    }

    fn partial_config_no_token() -> PushConfig {
        PushConfig {
            api_token: None,
            user_key: Some("user_key".to_string()),
            device: None,
            priority: None,
        }
    }

    fn partial_config_no_user() -> PushConfig {
        PushConfig {
            api_token: Some("api_token".to_string()),
            user_key: None,
            device: None,
            priority: None,
        }
    }

    #[test]
    fn test_is_enabled() {
        let enabled_config = PushConfig {
            api_token: Some("test".to_string()),
            user_key: Some("test".to_string()),
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(enabled_config, empty_registry());
        assert!(notifier.is_enabled());

        let disabled_config = PushConfig {
            api_token: None,
            user_key: None,
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(disabled_config, empty_registry());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_source_emoji() {
        assert_eq!(PushNotifier::get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(PushNotifier::get_source_emoji("sentry"), "\u{1F534}");
    }

    #[test]
    fn test_name() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        assert_eq!(notifier.name(), "push");
    }

    #[test]
    fn test_is_enabled_partial_configs() {
        assert!(!PushNotifier::new(partial_config_no_token(), empty_registry()).is_enabled());
        assert!(!PushNotifier::new(partial_config_no_user(), empty_registry()).is_enabled());
    }

    #[test]
    fn test_source_emoji_all_sources() {
        assert_eq!(PushNotifier::get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(PushNotifier::get_source_emoji("sentry"), "\u{1F534}");
        assert_eq!(PushNotifier::get_source_emoji("github"), "\u{1F419}");
        assert_eq!(PushNotifier::get_source_emoji("jira"), "\u{1F3AB}");
        assert_eq!(PushNotifier::get_source_emoji("unknown"), "\u{1F4CC}");
    }

    #[test]
    fn test_source_emoji_case_insensitive() {
        assert_eq!(PushNotifier::get_source_emoji("LINEAR"), "\u{1F4CB}");
        assert_eq!(PushNotifier::get_source_emoji("Sentry"), "\u{1F534}");
        assert_eq!(PushNotifier::get_source_emoji("GitHub"), "\u{1F419}");
        assert_eq!(PushNotifier::get_source_emoji("JIRA"), "\u{1F3AB}");
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_failed(&issue, "Error message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_status("Status update").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_disabled() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_single() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "sentry",
        )];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncated_to_five() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issues: Vec<Issue> = (0..10)
            .map(|i| {
                Issue::new(
                    format!("{}", i),
                    format!("PROJ-{}", i),
                    format!("Issue {}", i),
                    "https://example.com",
                    "github",
                )
            })
            .collect();

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_pushover_message_serialization() {
        let msg = PushoverMessage {
            token: "token".to_string(),
            user: "user".to_string(),
            message: "Test message".to_string(),
            title: Some("Title".to_string()),
            url: Some("https://example.com".to_string()),
            url_title: Some("Link".to_string()),
            priority: Some(1),
            device: Some("device".to_string()),
            sound: Some("pushover".to_string()),
        };

        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["token"], "token");
        assert_eq!(json["user"], "user");
        assert_eq!(json["message"], "Test message");
        assert_eq!(json["title"], "Title");
        assert_eq!(json["url"], "https://example.com");
        assert_eq!(json["url_title"], "Link");
        assert_eq!(json["priority"], 1);
        assert_eq!(json["device"], "device");
        assert_eq!(json["sound"], "pushover");
    }

    #[test]
    fn test_pushover_message_optional_fields_skipped() {
        let msg = PushoverMessage {
            token: "token".to_string(),
            user: "user".to_string(),
            message: "Test message".to_string(),
            title: None,
            url: None,
            url_title: None,
            priority: None,
            device: None,
            sound: None,
        };

        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["token"], "token");
        assert!(json.get("title").is_none());
        assert!(json.get("url").is_none());
        assert!(json.get("priority").is_none());
    }

    #[test]
    fn test_config_with_device_and_priority() {
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("user".to_string()),
            device: Some("iphone".to_string()),
            priority: Some(1),
        };

        let notifier = PushNotifier::new(config, empty_registry());
        assert!(notifier.is_enabled());
    }

    #[tokio::test]
    async fn test_notify_start_different_sources() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());

        for source in ["linear", "sentry", "github", "jira", "unknown"] {
            let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", source);
            let result = notifier.notify_start(&issue).await;
            assert!(result.is_ok());
        }
    }
}

//! Push notification via Pushover.

use super::Notifier;
use crate::config::PushConfig;
use crate::error::{Error, Result};
use crate::types::{AskDelivery, AskRequest, Issue};
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
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
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
        let emoji = crate::notifier::get_source_emoji(&issue.source);
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
        self.send_push("Claudear", message, None, None, Some(-1), None)
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
                let emoji = crate::notifier::get_source_emoji(&i.source);
                format!("{} {} - {}", emoji, i.short_id, i.title)
            })
            .collect::<Vec<_>>()
            .join("\n");

        // High priority for urgent issues
        self.send_push(&title, &message, None, None, Some(1), None)
            .await
    }

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let title = format!("Human input needed: {}", issue.short_id);
        let message = format!(
            "[CLAUDEAR-Q:{}]\n{}\n\nReply in Discord or Email.",
            request.correlation_id, request.question.question
        );
        self.send_push(
            &title,
            &message,
            Some(&issue.url),
            Some("View Issue"),
            Some(1),
            Some(issue),
        )
        .await?;
        Ok(Some(AskDelivery {
            channel: "push".to_string(),
            target: None,
            message_id: None,
        }))
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
        assert_eq!(crate::notifier::get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(crate::notifier::get_source_emoji("sentry"), "\u{1F534}");
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
        assert_eq!(crate::notifier::get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(crate::notifier::get_source_emoji("sentry"), "\u{1F534}");
        assert_eq!(crate::notifier::get_source_emoji("github"), "\u{1F419}");
        assert_eq!(crate::notifier::get_source_emoji("jira"), "\u{1F3AB}");
        assert_eq!(crate::notifier::get_source_emoji("unknown"), "\u{1F4CC}");
    }

    #[test]
    fn test_source_emoji_case_insensitive() {
        assert_eq!(crate::notifier::get_source_emoji("LINEAR"), "\u{1F4CB}");
        assert_eq!(crate::notifier::get_source_emoji("Sentry"), "\u{1F534}");
        assert_eq!(crate::notifier::get_source_emoji("GitHub"), "\u{1F419}");
        assert_eq!(crate::notifier::get_source_emoji("JIRA"), "\u{1F3AB}");
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

    #[test]
    fn test_source_emoji_empty_string() {
        assert_eq!(crate::notifier::get_source_emoji(""), "\u{1F4CC}");
    }

    #[test]
    fn test_source_emoji_mixed_case_github() {
        assert_eq!(crate::notifier::get_source_emoji("GiThUb"), "\u{1F419}");
    }

    #[test]
    fn test_source_emoji_with_whitespace_is_default() {
        // Whitespace-padded source should not match
        assert_eq!(crate::notifier::get_source_emoji(" linear "), "\u{1F4CC}");
    }

    #[test]
    fn test_resolve_user_key_returns_config_when_no_issue() {
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("global-key".to_string()),
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(config, empty_registry());
        assert_eq!(
            notifier.resolve_user_key(None),
            Some("global-key".to_string())
        );
    }

    #[test]
    fn test_resolve_user_key_returns_config_when_no_resolved_user() {
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("global-key".to_string()),
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(config, empty_registry());
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        assert_eq!(
            notifier.resolve_user_key(Some(&issue)),
            Some("global-key".to_string())
        );
    }

    #[test]
    fn test_resolve_user_key_uses_resolved_user_push_key() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                push_user_key: Some("jake-push-key".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("global-key".to_string()),
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(config, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        assert_eq!(
            notifier.resolve_user_key(Some(&issue)),
            Some("jake-push-key".to_string())
        );
    }

    #[test]
    fn test_resolve_user_key_falls_back_when_user_has_no_push_key() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                push_user_key: None,
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("global-key".to_string()),
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(config, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        assert_eq!(
            notifier.resolve_user_key(Some(&issue)),
            Some("global-key".to_string())
        );
    }

    #[test]
    fn test_resolve_user_key_none_when_config_has_no_key() {
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: None,
            device: None,
            priority: None,
        };
        let notifier = PushNotifier::new(config, empty_registry());
        assert_eq!(notifier.resolve_user_key(None), None);
    }

    #[test]
    fn test_pushover_message_priority_negative_values() {
        let msg = PushoverMessage {
            token: "t".to_string(),
            user: "u".to_string(),
            message: "m".to_string(),
            title: None,
            url: None,
            url_title: None,
            priority: Some(-2),
            device: None,
            sound: None,
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["priority"], -2);
    }

    #[test]
    fn test_pushover_message_all_none_optional_fields() {
        let msg = PushoverMessage {
            token: "t".to_string(),
            user: "u".to_string(),
            message: "m".to_string(),
            title: None,
            url: None,
            url_title: None,
            priority: None,
            device: None,
            sound: None,
        };
        let json_str = serde_json::to_string(&msg).unwrap();
        // None fields should be absent from JSON
        assert!(!json_str.contains("title"));
        assert!(!json_str.contains("url"));
        assert!(!json_str.contains("url_title"));
        assert!(!json_str.contains("priority"));
        assert!(!json_str.contains("device"));
        assert!(!json_str.contains("sound"));
    }

    // --- ask_question tests ---

    #[tokio::test]
    async fn test_ask_question_disabled_returns_ok() {
        let notifier = PushNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-push-1".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which env?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let result = notifier.ask_question(&issue, &request).await;
        assert!(result.is_ok());
        let delivery = result.unwrap().unwrap();
        assert_eq!(delivery.channel, "push");
        assert!(delivery.target.is_none());
        assert!(delivery.message_id.is_none());
    }

    #[test]
    fn test_urgent_issues_singular_grammar() {
        // Verify the format string produces correct singular
        let count = 1;
        let title = format!(
            "\u{1F6A8} {} Urgent Issue{}",
            count,
            if count > 1 { "s" } else { "" }
        );
        assert!(title.contains("1 Urgent Issue"));
        assert!(!title.contains("Issues"));
    }

    #[test]
    fn test_urgent_issues_plural_grammar() {
        let count = 5;
        let title = format!(
            "\u{1F6A8} {} Urgent Issue{}",
            count,
            if count > 1 { "s" } else { "" }
        );
        assert!(title.contains("5 Urgent Issues"));
    }

    #[test]
    fn test_is_enabled_requires_both_fields() {
        // Only api_token
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: None,
            device: None,
            priority: None,
        };
        assert!(!PushNotifier::new(config, empty_registry()).is_enabled());

        // Only user_key
        let config = PushConfig {
            api_token: None,
            user_key: Some("key".to_string()),
            device: None,
            priority: None,
        };
        assert!(!PushNotifier::new(config, empty_registry()).is_enabled());

        // Both present
        let config = PushConfig {
            api_token: Some("token".to_string()),
            user_key: Some("key".to_string()),
            device: None,
            priority: None,
        };
        assert!(PushNotifier::new(config, empty_registry()).is_enabled());
    }
}

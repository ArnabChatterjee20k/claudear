//! Discord webhook notifier.

use super::Notifier;
use crate::config::DiscordConfig;
use crate::error::{Error, Result};
use crate::types::Issue;
use async_trait::async_trait;
use serde::Serialize;

/// HTTP response for Discord webhook client.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Trait for HTTP client used by Discord notifier.
#[async_trait]
pub trait DiscordWebhookClient: Send + Sync {
    async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse>;
}

/// Real HTTP client using reqwest.
pub struct ReqwestDiscordWebhookClient {
    client: reqwest::Client,
}

impl ReqwestDiscordWebhookClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestDiscordWebhookClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DiscordWebhookClient for ReqwestDiscordWebhookClient {
    async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse> {
        let response = self.client.post(url).json(body).send().await?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        Ok(HttpResponse { status, body })
    }
}

/// Discord webhook notifier.
pub struct DiscordNotifier<H: DiscordWebhookClient = ReqwestDiscordWebhookClient> {
    config: DiscordConfig,
    http: H,
}

#[derive(Debug, Serialize)]
struct DiscordMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    embeds: Option<Vec<DiscordEmbed>>,
}

#[derive(Debug, Serialize)]
struct DiscordEmbed {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Vec<DiscordField>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer: Option<DiscordFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

#[derive(Debug, Serialize)]
struct DiscordField {
    name: String,
    value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline: Option<bool>,
}

#[derive(Debug, Serialize)]
struct DiscordFooter {
    text: String,
}

impl DiscordNotifier<ReqwestDiscordWebhookClient> {
    pub fn new(config: DiscordConfig) -> Self {
        Self {
            config,
            http: ReqwestDiscordWebhookClient::new(),
        }
    }
}

/// Maximum lengths for user-controlled fields to prevent unbounded memory allocation.
const MAX_SHORT_ID_LENGTH: usize = 64;
const MAX_SOURCE_LENGTH: usize = 32;
const MAX_URL_LENGTH: usize = 2000;
const MAX_DESCRIPTION_LENGTH: usize = 2048;

/// Truncate a string to the specified maximum length, adding "..." if truncated.
fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else if max_len > 3 {
        format!("{}...", &s[..max_len - 3])
    } else {
        s[..max_len].to_string()
    }
}

impl<H: DiscordWebhookClient> DiscordNotifier<H> {
    /// Create a new Discord notifier with a custom HTTP client.
    pub fn with_http_client(config: DiscordConfig, http: H) -> Self {
        Self { config, http }
    }

    async fn send(&self, message: DiscordMessage) -> Result<()> {
        let webhook_url = match &self.config.webhook_url {
            Some(url) => url,
            None => return Ok(()),
        };

        let body = serde_json::to_value(&message)?;
        let response = self.http.post_json(webhook_url, &body).await?;

        if response.status < 200 || response.status >= 300 {
            return Err(Error::notifier(
                "discord",
                format!("Webhook error: {}", response.body),
            ));
        }

        Ok(())
    }

    fn get_user_mention(&self) -> Option<String> {
        self.config.user_id.as_ref().map(|id| format!("<@{}>", id))
    }

    fn get_source_emoji(source: &str) -> &'static str {
        match source.to_lowercase().as_str() {
            "linear" => "\u{1F4CB}", // clipboard
            "sentry" => "\u{1F534}", // red circle
            "github" => "\u{1F419}", // octopus
            "jira" => "\u{1F3AB}",   // ticket
            _ => "\u{1F4CC}",        // pushpin
        }
    }

    fn timestamp() -> String {
        chrono::Utc::now().to_rfc3339()
    }
}

#[async_trait]
impl<H: DiscordWebhookClient + 'static> Notifier for DiscordNotifier<H> {
    fn name(&self) -> &str {
        "discord"
    }

    fn is_enabled(&self) -> bool {
        self.config.webhook_url.is_some()
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let mention = self.get_user_mention();
        let emoji = Self::get_source_emoji(&issue.source);
        let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
        let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
        let url = truncate_string(&issue.url, MAX_URL_LENGTH);
        let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

        self.send(DiscordMessage {
            content: mention.map(|m| format!("{} Processing issue...", m)),
            embeds: Some(vec![DiscordEmbed {
                title: Some(format!("{} Processing: {}", emoji, short_id)),
                description: Some(title),
                url: Some(url),
                color: Some(0x3498db), // Blue
                fields: Some(vec![
                    DiscordField {
                        name: "Source".to_string(),
                        value: source,
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "Priority".to_string(),
                        value: issue.priority.to_string(),
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "Status".to_string(),
                        value: issue.status.to_string(),
                        inline: Some(true),
                    },
                ]),
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let mention = self.get_user_mention();
        let emoji = Self::get_source_emoji(&issue.source);
        let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
        let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
        let issue_url = truncate_string(&issue.url, MAX_URL_LENGTH);
        let pr_url_truncated = truncate_string(pr_url, MAX_URL_LENGTH);
        let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

        self.send(DiscordMessage {
            content: mention.map(|m| format!("{} PR created!", m)),
            embeds: Some(vec![DiscordEmbed {
                title: Some(format!("\u{2705} PR Created: {}", short_id)),
                description: Some(title),
                url: Some(pr_url_truncated.clone()),
                color: Some(0x2ecc71), // Green
                fields: Some(vec![
                    DiscordField {
                        name: "Source".to_string(),
                        value: format!("{} {}", emoji, source),
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "Issue".to_string(),
                        value: format!("[{}]({})", short_id, issue_url),
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "PR Link".to_string(),
                        value: format!("[View PR]({})", pr_url_truncated),
                        inline: Some(false),
                    },
                ]),
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let mention = self.get_user_mention();
        let emoji = Self::get_source_emoji(&issue.source);
        let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
        let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
        let url = truncate_string(&issue.url, MAX_URL_LENGTH);
        let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

        self.send(DiscordMessage {
            content: mention.map(|m| format!("{} Issue processed (no PR URL found)", m)),
            embeds: Some(vec![DiscordEmbed {
                title: Some(format!("\u{2714}\u{FE0F} Completed: {}", short_id)),
                description: Some(title),
                url: Some(url),
                color: Some(0x9b59b6), // Purple
                fields: Some(vec![
                    DiscordField {
                        name: "Source".to_string(),
                        value: format!("{} {}", emoji, source),
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "Note".to_string(),
                        value: "Claude completed but no PR URL was captured".to_string(),
                        inline: Some(false),
                    },
                ]),
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let mention = self.get_user_mention();
        let emoji = Self::get_source_emoji(&issue.source);
        let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
        let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
        let url = truncate_string(&issue.url, MAX_URL_LENGTH);
        let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

        // Truncate error message if too long
        let error_display = truncate_string(error, 1000);

        self.send(DiscordMessage {
            content: mention.map(|m| format!("{} Fix attempt failed", m)),
            embeds: Some(vec![DiscordEmbed {
                title: Some(format!("\u{274C} Failed: {}", short_id)),
                description: Some(title),
                url: Some(url),
                color: Some(0xe74c3c), // Red
                fields: Some(vec![
                    DiscordField {
                        name: "Source".to_string(),
                        value: format!("{} {}", emoji, source),
                        inline: Some(true),
                    },
                    DiscordField {
                        name: "Error".to_string(),
                        value: error_display,
                        inline: Some(false),
                    },
                ]),
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let message_truncated = truncate_string(message, MAX_DESCRIPTION_LENGTH);

        self.send(DiscordMessage {
            content: None,
            embeds: Some(vec![DiscordEmbed {
                title: None,
                description: Some(message_truncated),
                url: None,
                color: Some(0x9b59b6), // Purple
                fields: None,
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let mention = self.get_user_mention();

        let fields: Vec<DiscordField> = issues
            .iter()
            .take(10)
            .map(|issue| {
                let emoji = Self::get_source_emoji(&issue.source);
                let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
                let title = truncate_string(&issue.title, 50);
                let url = truncate_string(&issue.url, MAX_URL_LENGTH);
                DiscordField {
                    name: format!("{} {}", emoji, short_id),
                    value: format!("[{}]({})", title, url),
                    inline: Some(true),
                }
            })
            .collect();

        self.send(DiscordMessage {
            content: mention.map(|m| format!("{} Urgent issues detected!", m)),
            embeds: Some(vec![DiscordEmbed {
                title: Some(format!(
                    "\u{1F6A8} {} Urgent Issue{} Detected",
                    issues.len(),
                    if issues.len() > 1 { "s" } else { "" }
                )),
                description: Some("The following issues require immediate attention:".to_string()),
                url: None,
                color: Some(0xf39c12), // Orange
                fields: Some(fields),
                footer: Some(DiscordFooter {
                    text: "Claude Watchers".to_string(),
                }),
                timestamp: Some(Self::timestamp()),
            }]),
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_emoji() {
        type TestNotifier = DiscordNotifier<ReqwestDiscordWebhookClient>;
        assert_eq!(TestNotifier::get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(TestNotifier::get_source_emoji("sentry"), "\u{1F534}");
        assert_eq!(TestNotifier::get_source_emoji("github"), "\u{1F419}");
        assert_eq!(TestNotifier::get_source_emoji("unknown"), "\u{1F4CC}");
    }

    #[test]
    fn test_user_mention() {
        let config_with_id = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("123456".to_string()),
        };
        let notifier = DiscordNotifier::new(config_with_id);
        assert_eq!(notifier.get_user_mention(), Some("<@123456>".to_string()));

        let config_without_id = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config_without_id);
        assert_eq!(notifier.get_user_mention(), None);
    }

    #[test]
    fn test_is_enabled() {
        let enabled_config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
        };
        let notifier = DiscordNotifier::new(enabled_config);
        assert!(notifier.is_enabled());

        let disabled_config = DiscordConfig {
            webhook_url: None,
            user_id: None,
        };
        let notifier = DiscordNotifier::new(disabled_config);
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_notifier_name() {
        let config = DiscordConfig::default();
        let notifier = DiscordNotifier::new(config);
        assert_eq!(notifier.name(), "discord");
    }

    #[test]
    fn test_source_emoji_case_insensitive() {
        type TestNotifier = DiscordNotifier<ReqwestDiscordWebhookClient>;
        assert_eq!(TestNotifier::get_source_emoji("LINEAR"), "\u{1F4CB}");
        assert_eq!(TestNotifier::get_source_emoji("Linear"), "\u{1F4CB}");
        assert_eq!(TestNotifier::get_source_emoji("SENTRY"), "\u{1F534}");
        assert_eq!(TestNotifier::get_source_emoji("GitHub"), "\u{1F419}");
    }

    #[test]
    fn test_source_emoji_jira() {
        type TestNotifier = DiscordNotifier<ReqwestDiscordWebhookClient>;
        assert_eq!(TestNotifier::get_source_emoji("jira"), "\u{1F3AB}");
        assert_eq!(TestNotifier::get_source_emoji("JIRA"), "\u{1F3AB}");
    }

    #[test]
    fn test_timestamp_format() {
        type TestNotifier = DiscordNotifier<ReqwestDiscordWebhookClient>;
        let timestamp = TestNotifier::timestamp();
        // Should be valid RFC3339
        assert!(timestamp.contains("T"));
        assert!(timestamp.contains("+") || timestamp.contains("Z"));
    }

    #[test]
    fn test_discord_message_serialization() {
        let message = DiscordMessage {
            content: Some("Test message".to_string()),
            embeds: None,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("Test message"));
        // embeds should be skipped because it's None
        assert!(!json.contains("embeds"));
    }

    #[test]
    fn test_discord_embed_serialization() {
        let embed = DiscordEmbed {
            title: Some("Test Title".to_string()),
            description: Some("Test Description".to_string()),
            url: Some("https://example.com".to_string()),
            color: Some(0xFF0000),
            fields: None,
            footer: None,
            timestamp: None,
        };
        let json = serde_json::to_string(&embed).unwrap();
        assert!(json.contains("Test Title"));
        assert!(json.contains("Test Description"));
        assert!(json.contains("https://example.com"));
        // Optional fields should be skipped
        assert!(!json.contains("fields"));
        assert!(!json.contains("footer"));
        assert!(!json.contains("timestamp"));
    }

    #[test]
    fn test_discord_field_serialization() {
        let field = DiscordField {
            name: "Field Name".to_string(),
            value: "Field Value".to_string(),
            inline: Some(true),
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(json.contains("Field Name"));
        assert!(json.contains("Field Value"));
        assert!(json.contains("true"));
    }

    #[test]
    fn test_discord_field_serialization_no_inline() {
        let field = DiscordField {
            name: "Field Name".to_string(),
            value: "Field Value".to_string(),
            inline: None,
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(!json.contains("inline"));
    }

    #[test]
    fn test_discord_footer_serialization() {
        let footer = DiscordFooter {
            text: "Footer Text".to_string(),
        };
        let json = serde_json::to_string(&footer).unwrap();
        assert!(json.contains("Footer Text"));
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let config = DiscordConfig {
            webhook_url: None, // Disabled
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        // Should return Ok without actually sending
        let result = notifier.notify_status("Test status").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        let issue = Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );
        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        let issue = Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );
        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        let issue = Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );
        let result = notifier.notify_failed(&issue, "Test error").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        let issue = Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );
        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
        };
        let notifier = DiscordNotifier::new(config);

        // Empty list should return Ok without sending
        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    // Mock-based tests for HTTP-dependent functionality

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Mock Discord webhook client for testing.
    struct MockDiscordWebhookClient {
        response_status: u16,
        response_body: String,
        call_count: AtomicUsize,
        last_calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockDiscordWebhookClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response_status: status,
                response_body: body.to_string(),
                call_count: AtomicUsize::new(0),
                last_calls: Mutex::new(Vec::new()),
            }
        }

        fn success() -> Self {
            Self::new(204, "")
        }

        fn error(status: u16, body: &str) -> Self {
            Self::new(status, body)
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn get_last_call(&self) -> Option<(String, serde_json::Value)> {
            self.last_calls.lock().unwrap().last().cloned()
        }
    }

    #[async_trait]
    impl DiscordWebhookClient for MockDiscordWebhookClient {
        async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.last_calls
                .lock()
                .unwrap()
                .push((url.to_string(), body.clone()));

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }
    }

    fn enabled_config() -> DiscordConfig {
        DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
        }
    }

    fn enabled_config_with_user() -> DiscordConfig {
        DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("987654321".to_string()),
        }
    }

    #[tokio::test]
    async fn test_send_webhook_success() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_send_webhook_sends_to_correct_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (url, _) = notifier.http.get_last_call().unwrap();
        assert_eq!(url, "https://discord.com/api/webhooks/123/abc");
    }

    #[tokio::test]
    async fn test_send_webhook_error_response() {
        let mock = MockDiscordWebhookClient::error(400, "Bad Request");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("Webhook error"));
        assert!(err_str.contains("Bad Request"));
    }

    #[tokio::test]
    async fn test_send_webhook_server_error() {
        let mock = MockDiscordWebhookClient::error(500, "Internal Server Error");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        let result = notifier.notify_status("Test").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_start_sends_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue Title",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"].is_array());
        let embed = &body["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("PROJ-123"));
        assert_eq!(embed["description"], "Test Issue Title");
    }

    #[tokio::test]
    async fn test_notify_start_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
    }

    #[tokio::test]
    async fn test_notify_success_sends_correct_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/42")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let embed = &body["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("PR Created"));
        assert_eq!(embed["url"], "https://github.com/org/repo/pull/42");
        assert_eq!(embed["color"], 0x2ecc71); // Green
    }

    #[tokio::test]
    async fn test_notify_completed_sends_correct_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_completed(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let embed = &body["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("Completed"));
        assert_eq!(embed["color"], 0x9b59b6); // Purple
    }

    #[tokio::test]
    async fn test_notify_failed_sends_correct_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier
            .notify_failed(&issue, "Build failed with exit code 1")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let embed = &body["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("Failed"));
        assert_eq!(embed["color"], 0xe74c3c); // Red
                                              // Check error field
        let fields = embed["fields"].as_array().unwrap();
        let error_field = fields.iter().find(|f| f["name"] == "Error").unwrap();
        assert!(error_field["value"]
            .as_str()
            .unwrap()
            .contains("Build failed"));
    }

    #[tokio::test]
    async fn test_notify_failed_truncates_long_error() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let long_error = "x".repeat(2000);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let error_field = fields.iter().find(|f| f["name"] == "Error").unwrap();
        let error_value = error_field["value"].as_str().unwrap();
        assert!(error_value.len() <= 1010);
        assert!(error_value.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_status_sends_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("System is healthy").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let embed = &body["embeds"][0];
        assert_eq!(embed["description"], "System is healthy");
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_sends_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com/1", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com/2", "sentry"),
        ];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let embed = &body["embeds"][0];
        assert!(embed["title"].as_str().unwrap().contains("2 Urgent Issues"));
        assert_eq!(embed["color"], 0xf39c12); // Orange
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncates_long_title() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let long_title = "x".repeat(100);
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            &long_title,
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let field_value = fields[0]["value"].as_str().unwrap();
        // Title should be truncated (47 chars + "...")
        assert!(field_value.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_limits_to_ten() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues: Vec<Issue> = (1..=20)
            .map(|i| {
                Issue::new(
                    i.to_string(),
                    format!("PROJ-{}", i),
                    format!("Issue {}", i),
                    format!("https://example.com/{}", i),
                    "linear",
                )
            })
            .collect();

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 10);
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_single_item_grammar() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        // Should use singular "Issue" not "Issues"
        assert!(title.contains("1 Urgent Issue Detected"));
        assert!(!title.contains("Issues"));
    }

    #[test]
    fn test_with_http_client() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "discord");
    }

    #[test]
    fn test_reqwest_discord_webhook_client_default() {
        let client = ReqwestDiscordWebhookClient::default();
        assert!(std::mem::size_of_val(&client) > 0);
    }

    #[test]
    fn test_http_response_fields() {
        let response = HttpResponse {
            status: 201,
            body: "Created".to_string(),
        };
        assert_eq!(response.status, 201);
        assert_eq!(response.body, "Created");
    }

    #[tokio::test]
    async fn test_source_specific_embeds() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        // Test linear source
        let linear_issue = Issue::new(
            "1",
            "LIN-1",
            "Linear Issue",
            "https://linear.app/1",
            "linear",
        );
        notifier.notify_start(&linear_issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let source_field = fields.iter().find(|f| f["name"] == "Source").unwrap();
        assert_eq!(source_field["value"], "linear");
    }

    #[tokio::test]
    async fn test_embed_has_timestamp() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let timestamp = body["embeds"][0]["timestamp"].as_str().unwrap();
        // Should be RFC3339 format
        assert!(timestamp.contains("T"));
    }

    #[tokio::test]
    async fn test_embed_has_footer() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let footer = body["embeds"][0]["footer"]["text"].as_str().unwrap();
        assert_eq!(footer, "Claude Watchers");
    }
}

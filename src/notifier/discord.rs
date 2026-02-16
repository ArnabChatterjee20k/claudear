//! Discord webhook notifier.

use super::Notifier;
use crate::config::DiscordConfig;
use crate::discord::DiscordClient;
use crate::error::{Error, Result};
use crate::http::HttpResponse;
use crate::types::{AskDelivery, AskReply, AskRequest, Issue};
use crate::users::UserRegistry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

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
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
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
    user_registry: UserRegistry,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscordMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) embeds: Option<Vec<DiscordEmbed>>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscordEmbed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fields: Option<Vec<DiscordField>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) footer: Option<DiscordFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) timestamp: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscordField {
    pub(crate) name: String,
    pub(crate) value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) inline: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DiscordFooter {
    pub(crate) text: String,
}

impl DiscordNotifier<ReqwestDiscordWebhookClient> {
    pub fn new(config: DiscordConfig, user_registry: UserRegistry) -> Self {
        Self {
            config,
            http: ReqwestDiscordWebhookClient::new(),
            user_registry,
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
        format!("{}...", &s[..s.floor_char_boundary(max_len - 3)])
    } else {
        s[..s.floor_char_boundary(max_len)].to_string()
    }
}

impl<H: DiscordWebhookClient> DiscordNotifier<H> {
    /// Create a new Discord notifier with a custom HTTP client.
    pub fn with_http_client(config: DiscordConfig, http: H) -> Self {
        Self {
            config,
            http,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
        }
    }

    /// Create a new Discord notifier with a custom HTTP client and user registry.
    pub fn with_http_client_and_registry(
        config: DiscordConfig,
        http: H,
        user_registry: UserRegistry,
    ) -> Self {
        Self {
            config,
            http,
            user_registry,
        }
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

    fn get_user_mention_for_issue(&self, issue: &Issue) -> Option<String> {
        // Check for resolved user first
        if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
            if let Some(user) = self.user_registry.get_by_slug(&slug) {
                if let Some(ref discord_id) = user.discord_id {
                    return Some(format!("<@{}>", discord_id));
                }
            }
        }
        // Fall back to global config
        self.config.user_id.as_ref().map(|id| format!("<@{}>", id))
    }

    fn get_target_discord_id_for_issue(&self, issue: &Issue) -> Option<String> {
        if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
            if let Some(user) = self.user_registry.get_by_slug(&slug) {
                if let Some(ref discord_id) = user.discord_id {
                    return Some(discord_id.clone());
                }
            }
        }
        self.config.user_id.clone()
    }

    fn expected_reply_user_id(&self, request: &AskRequest) -> Option<String> {
        request
            .target_discord_id
            .clone()
            .or_else(|| self.config.user_id.clone())
    }

    fn extract_reply_text(content: &str) -> Option<String> {
        let answer = content.trim();
        if answer.is_empty() {
            None
        } else {
            Some(answer.to_string())
        }
    }

    fn extract_reply_text_with_token(content: &str, correlation_id: &str) -> Option<String> {
        let token = format!("[CLAUDEAR-Q:{}]", correlation_id);
        if !content.contains(&token) {
            return None;
        }
        let cleaned = content.replace(&token, "");
        Self::extract_reply_text(&cleaned)
    }
}

// Re-export the shared emoji function for backward compatibility within this module.
pub(crate) use super::get_source_emoji;

/// Return the current UTC timestamp in RFC 3339 format.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Build the Discord message for a "processing started" notification.
pub(crate) fn build_start_message(issue: &Issue, mention: Option<String>) -> DiscordMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    DiscordMessage {
        content: mention.map(|m| m.to_string()),
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
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    }
}

/// Build the Discord message for a "PR created" notification.
pub(crate) fn build_success_message(
    issue: &Issue,
    pr_url: &str,
    mention: Option<String>,
) -> DiscordMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let issue_url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let pr_url_truncated = truncate_string(pr_url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    DiscordMessage {
        content: mention.map(|m| m.to_string()),
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
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    }
}

/// Build the Discord message for a "completed without PR" notification.
pub(crate) fn build_completed_message(issue: &Issue, mention: Option<String>) -> DiscordMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    DiscordMessage {
        content: mention.map(|m| m.to_string()),
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
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    }
}

/// Build the Discord message for a "failed" notification.
pub(crate) fn build_failed_message(
    issue: &Issue,
    error: &str,
    mention: Option<String>,
) -> DiscordMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);
    let error_display = truncate_string(error, 1000);

    DiscordMessage {
        content: mention.map(|m| m.to_string()),
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
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    }
}

/// Build the Discord message for a status notification.
pub(crate) fn build_status_message(message: &str) -> DiscordMessage {
    let message_truncated = truncate_string(message, MAX_DESCRIPTION_LENGTH);

    DiscordMessage {
        content: None,
        embeds: Some(vec![DiscordEmbed {
            title: None,
            description: Some(message_truncated),
            url: None,
            color: Some(0x9b59b6), // Purple
            fields: None,
            footer: Some(DiscordFooter {
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    }
}

/// Build the Discord message for an "urgent issues" notification.
///
/// Returns `None` when the issue list is empty (nothing to send).
pub(crate) fn build_urgent_issues_message(
    issues: &[Issue],
    mention: Option<String>,
) -> Option<DiscordMessage> {
    if issues.is_empty() {
        return None;
    }

    let fields: Vec<DiscordField> = issues
        .iter()
        .take(10)
        .map(|issue| {
            let emoji = get_source_emoji(&issue.source);
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

    Some(DiscordMessage {
        content: mention.map(|m| m.to_string()),
        embeds: Some(vec![DiscordEmbed {
            title: Some(format!(
                "\u{1F6A8} {} Urgent Issue{} Detected",
                issues.len(),
                if issues.len() > 1 { "s" } else { "" }
            )),
            description: Some("The following issues require attention:".to_string()),
            url: None,
            color: Some(0xf39c12), // Orange
            fields: Some(fields),
            footer: Some(DiscordFooter {
                text: "Claudear".to_string(),
            }),
            timestamp: Some(timestamp()),
        }]),
    })
}

/// Build the Discord message for a human-in-the-loop question.
pub(crate) fn build_ask_question_message(
    issue: &Issue,
    request: &AskRequest,
    mention: Option<String>,
) -> DiscordMessage {
    let token = format!("[CLAUDEAR-Q:{}]", request.correlation_id);
    let mut content = String::new();
    if let Some(m) = mention {
        content.push_str(&m);
        content.push(' ');
    }
    content.push_str(&format!(
        "{} Human input needed for {}:\n{}",
        token, issue.short_id, request.question.question
    ));
    if let Some(ref why) = request.question.why {
        content.push_str(&format!("\nWhy: {}", why));
    }
    if let Some(ref ctx) = request.question.context {
        content.push_str(&format!("\nContext: {}", truncate_string(ctx, 400)));
    }
    if !request.question.options.is_empty() {
        content.push_str(&format!(
            "\nOptions: {}",
            request.question.options.join(" | ")
        ));
    }
    content.push_str("\nReply to this message in Discord with your answer.");

    DiscordMessage {
        content: Some(content),
        embeds: None,
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
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_start_message(issue, mention)).await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_success_message(issue, pr_url, mention))
            .await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_completed_message(issue, mention)).await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_failed_message(issue, error, mention)).await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        self.send(build_status_message(message)).await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        let mention = self.get_user_mention();
        match build_urgent_issues_message(issues, mention) {
            Some(message) => self.send(message).await,
            None => Ok(()),
        }
    }

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_ask_question_message(issue, request, mention))
            .await?;

        Ok(Some(AskDelivery {
            channel: "discord".to_string(),
            target: self.get_target_discord_id_for_issue(issue),
            message_id: None,
        }))
    }

    async fn poll_question_replies(
        &self,
        request: &AskRequest,
        since: DateTime<Utc>,
    ) -> Result<Vec<AskReply>> {
        let bot_token = match self.config.bot_token.as_ref() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(Vec::new()),
        };
        let channel_id = match self.config.channel_id.as_ref() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(Vec::new()),
        };

        let client = DiscordClient::new(bot_token.clone())?;
        let messages = client.list_channel_messages(channel_id, 50).await?;
        let expected_user = self.expected_reply_user_id(request);
        let token = format!("[CLAUDEAR-Q:{}]", request.correlation_id);

        let ask_message_ids: std::collections::HashSet<String> = messages
            .iter()
            .filter(|m| m.content.contains(&token))
            .map(|m| m.id.clone())
            .collect();

        let mut replies: Vec<AskReply> = messages
            .into_iter()
            .filter_map(|message| {
                let author = message.author?;
                if author.bot {
                    return None;
                }

                if let Some(ref expected) = expected_user {
                    if &author.id != expected {
                        return None;
                    }
                }

                let parsed = DateTime::parse_from_rfc3339(&message.timestamp)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))?;
                if parsed < since {
                    return None;
                }

                let is_reply_to_ask = message
                    .message_reference
                    .as_ref()
                    .and_then(|r| r.message_id.as_ref())
                    .map(|message_id| ask_message_ids.contains(message_id))
                    .unwrap_or(false);

                let answer = if is_reply_to_ask {
                    Self::extract_reply_text(&message.content)
                } else {
                    // Backward-compatible fallback for manual token replies.
                    Self::extract_reply_text_with_token(&message.content, &request.correlation_id)
                }?;
                Some(AskReply {
                    correlation_id: request.correlation_id.clone(),
                    channel: "discord".to_string(),
                    responder: Some(author.id),
                    answer,
                    replied_at: parsed,
                })
            })
            .collect();

        replies.sort_by_key(|r| r.replied_at);
        Ok(replies)
    }

    fn supports_replies(&self) -> bool {
        self.config
            .bot_token
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
            && self
                .config
                .channel_id
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_registry() -> crate::users::UserRegistry {
        crate::users::UserRegistry::new(std::collections::HashMap::new())
    }

    #[test]
    fn test_source_emoji() {
        assert_eq!(get_source_emoji("linear"), "\u{1F4CB}");
        assert_eq!(get_source_emoji("sentry"), "\u{1F534}");
        assert_eq!(get_source_emoji("github"), "\u{1F419}");
        assert_eq!(get_source_emoji("unknown"), "\u{1F4CC}");
    }

    #[test]
    fn test_user_mention() {
        let config_with_id = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("123456".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config_with_id, empty_registry());
        assert_eq!(notifier.get_user_mention(), Some("<@123456>".to_string()));

        let config_without_id = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config_without_id, empty_registry());
        assert_eq!(notifier.get_user_mention(), None);
    }

    #[test]
    fn test_is_enabled() {
        let enabled_config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(enabled_config, empty_registry());
        assert!(notifier.is_enabled());

        let disabled_config = DiscordConfig {
            webhook_url: None,
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(disabled_config, empty_registry());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_notifier_name() {
        let config = DiscordConfig::default();
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert_eq!(notifier.name(), "discord");
    }

    #[test]
    fn test_source_emoji_case_insensitive() {
        assert_eq!(get_source_emoji("LINEAR"), "\u{1F4CB}");
        assert_eq!(get_source_emoji("Linear"), "\u{1F4CB}");
        assert_eq!(get_source_emoji("SENTRY"), "\u{1F534}");
        assert_eq!(get_source_emoji("GitHub"), "\u{1F419}");
    }

    #[test]
    fn test_source_emoji_jira() {
        assert_eq!(get_source_emoji("jira"), "\u{1F3AB}");
        assert_eq!(get_source_emoji("JIRA"), "\u{1F3AB}");
    }

    #[test]
    fn test_timestamp_format() {
        let ts = timestamp();
        // Should be valid RFC3339
        assert!(ts.contains("T"));
        assert!(ts.contains("+") || ts.contains("Z"));
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
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

        // Should return Ok without actually sending
        let result = notifier.notify_status("Test status").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

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
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

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
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

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
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

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
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());

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
            ..Default::default()
        }
    }

    fn enabled_config_with_user() -> DiscordConfig {
        DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("987654321".to_string()),
            ..Default::default()
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
        assert_eq!(footer, "Claudear");
    }

    #[tokio::test]
    async fn test_notify_start_with_resolved_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                discord_id: Some("111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        notifier.notify_start(&issue).await.unwrap();
        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@111222333>"));
    }

    #[tokio::test]
    async fn test_resolved_user_overrides_global_user_id() {
        let mock = MockDiscordWebhookClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                discord_id: Some("111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("999999999".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        notifier.notify_start(&issue).await.unwrap();
        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@111222333>"));
        assert!(!content.contains("<@999999999>"));
    }

    #[tokio::test]
    async fn test_fallback_to_global_when_no_resolved_user() {
        let mock = MockDiscordWebhookClient::success();
        let registry = crate::users::UserRegistry::new(std::collections::HashMap::new());
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("999999999".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        notifier.notify_start(&issue).await.unwrap();
        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@999999999>"));
    }

    #[tokio::test]
    async fn test_ask_question_uses_resolved_user_target() {
        let mock = MockDiscordWebhookClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                discord_id: Some("111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("999999999".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");

        let request = crate::types::AskRequest {
            correlation_id: "tok-1".to_string(),
            source: "linear".to_string(),
            repo: Some("org/repo".to_string()),
            issue_id: issue.id.clone(),
            short_id: issue.short_id.clone(),
            question: crate::types::BlockingQuestion {
                question: "Choose target branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("111222333"));
    }

    #[tokio::test]
    async fn test_ask_question_falls_back_to_global_target() {
        let mock = MockDiscordWebhookClient::success();
        let registry = crate::users::UserRegistry::new(std::collections::HashMap::new());
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("999999999".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-2".to_string(),
            source: "linear".to_string(),
            repo: Some("org/repo".to_string()),
            issue_id: issue.id.clone(),
            short_id: issue.short_id.clone(),
            question: crate::types::BlockingQuestion {
                question: "Pick env?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("999999999"));
    }

    #[test]
    fn test_extract_reply_text() {
        let content = "Use main branch";
        let parsed =
            DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text(content).unwrap();
        assert_eq!(parsed, "Use main branch");
    }

    #[test]
    fn test_extract_reply_text_with_token() {
        let content = "[CLAUDEAR-Q:abc123] Use main branch";
        let parsed = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "abc123",
        )
        .unwrap();
        assert_eq!(parsed, "Use main branch");
    }

    #[test]
    fn test_truncate_string_short_unchanged() {
        assert_eq!(truncate_string("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_string_exact_length_unchanged() {
        assert_eq!(truncate_string("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_string_over_limit_adds_ellipsis() {
        let result = truncate_string("hello world", 8);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 8);
    }

    #[test]
    fn test_truncate_string_very_small_max_no_room_for_ellipsis() {
        // When max_len <= 3, no room for ellipsis so just truncate
        let result = truncate_string("hello", 3);
        assert_eq!(result.len(), 3);
        assert!(!result.contains("..."));
    }

    #[test]
    fn test_truncate_string_max_len_zero() {
        let result = truncate_string("hello", 0);
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_string_empty_input() {
        assert_eq!(truncate_string("", 10), "");
    }

    #[test]
    fn test_truncate_string_with_known_constants() {
        let long_id = "x".repeat(100);
        let result = truncate_string(&long_id, MAX_SHORT_ID_LENGTH);
        assert!(result.len() <= MAX_SHORT_ID_LENGTH);
        assert!(result.ends_with("..."));

        let long_source = "y".repeat(50);
        let result = truncate_string(&long_source, MAX_SOURCE_LENGTH);
        assert!(result.len() <= MAX_SOURCE_LENGTH);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_string_description_length() {
        let long_desc = "z".repeat(3000);
        let result = truncate_string(&long_desc, MAX_DESCRIPTION_LENGTH);
        assert!(result.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_string_url_length() {
        let long_url = format!("https://example.com/{}", "a".repeat(2500));
        let result = truncate_string(&long_url, MAX_URL_LENGTH);
        assert!(result.len() <= MAX_URL_LENGTH);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_extract_reply_text_empty_string() {
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text("");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_whitespace_only() {
        let result =
            DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text("   \n\t  ");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_trims_whitespace() {
        let result =
            DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text("  yes  ").unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_extract_reply_text_with_token_wrong_id_returns_none() {
        let content = "[CLAUDEAR-Q:abc123] Use main branch";
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "wrong-id",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_only_token_returns_none() {
        // Token present but no actual text after removing it
        let content = "[CLAUDEAR-Q:abc123]";
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "abc123",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_whitespace_after_token_returns_none() {
        let content = "[CLAUDEAR-Q:abc123]   ";
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "abc123",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_no_token_at_all() {
        let content = "just a regular message";
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "abc123",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_token_in_middle() {
        let content = "Before [CLAUDEAR-Q:abc123] After";
        let result = DiscordNotifier::<ReqwestDiscordWebhookClient>::extract_reply_text_with_token(
            content, "abc123",
        )
        .unwrap();
        assert_eq!(result, "Before  After");
    }

    #[test]
    fn test_supports_replies_true_when_both_set() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            bot_token: Some("bot-token".to_string()),
            channel_id: Some("channel-123".to_string()),
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert!(notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_bot_token() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            bot_token: None,
            channel_id: Some("channel-123".to_string()),
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_channel_id() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            bot_token: Some("bot-token".to_string()),
            channel_id: None,
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_empty_bot_token() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            bot_token: Some("".to_string()),
            channel_id: Some("channel-123".to_string()),
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_empty_channel_id() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            bot_token: Some("bot-token".to_string()),
            channel_id: Some("".to_string()),
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_expected_reply_user_id_prefers_request_target() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("global-user".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        let request = AskRequest {
            correlation_id: "tok-1".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: Some("request-target".to_string()),
            target_email: None,
        };
        assert_eq!(
            notifier.expected_reply_user_id(&request),
            Some("request-target".to_string())
        );
    }

    #[test]
    fn test_expected_reply_user_id_falls_back_to_config() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("global-user".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        let request = AskRequest {
            correlation_id: "tok-2".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        assert_eq!(
            notifier.expected_reply_user_id(&request),
            Some("global-user".to_string())
        );
    }

    #[test]
    fn test_expected_reply_user_id_none_when_both_absent() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        let request = AskRequest {
            correlation_id: "tok-3".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        assert_eq!(notifier.expected_reply_user_id(&request), None);
    }

    #[test]
    fn test_get_user_mention_for_issue_no_resolved_user_no_global() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::new(config, empty_registry());
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        assert_eq!(notifier.get_user_mention_for_issue(&issue), None);
    }

    #[test]
    fn test_get_user_mention_for_issue_resolved_user_no_discord_id() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                discord_id: None,
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("fallback".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(
            config,
            ReqwestDiscordWebhookClient::new(),
            registry,
        );
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        // Falls back to global because resolved user has no discord_id
        assert_eq!(
            notifier.get_user_mention_for_issue(&issue),
            Some("<@fallback>".to_string())
        );
    }

    #[tokio::test]
    async fn test_ask_question_includes_options_and_context() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test Issue", "https://example.com", "linear");
        let request = AskRequest {
            correlation_id: "tok-opts".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Pick a branch".to_string(),
                context: Some("We need a target for the PR".to_string()),
                options: vec!["main".to_string(), "develop".to_string()],
                why: Some("Multiple branches available".to_string()),
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("[CLAUDEAR-Q:tok-opts]"));
        assert!(content.contains("Pick a branch"));
        assert!(content.contains("Why: Multiple branches available"));
        assert!(content.contains("Context: We need a target for the PR"));
        assert!(content.contains("main | develop"));
    }

    #[tokio::test]
    async fn test_ask_question_delivery_channel() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = AskRequest {
            correlation_id: "tok-ch".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Question?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.channel, "discord");
        assert!(delivery.message_id.is_none());
    }

    // --- Additional tests for coverage ---

    fn make_ask_request(
        correlation_id: &str,
        question: &str,
        context: Option<&str>,
        options: Vec<&str>,
        why: Option<&str>,
        target_discord_id: Option<&str>,
    ) -> AskRequest {
        AskRequest {
            correlation_id: correlation_id.to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: question.to_string(),
                context: context.map(|s| s.to_string()),
                options: options.into_iter().map(|s| s.to_string()).collect(),
                why: why.map(|s| s.to_string()),
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: target_discord_id.map(|s| s.to_string()),
            target_email: None,
        }
    }

    #[tokio::test]
    async fn test_notify_start_sentry_source_uses_red_circle_emoji() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "SENTRY-1",
            "Sentry Error",
            "https://sentry.io/1",
            "sentry",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        assert!(title.contains("\u{1F534}"));
        assert!(title.contains("SENTRY-1"));
    }

    #[tokio::test]
    async fn test_notify_start_github_source_uses_octopus_emoji() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "GH-42",
            "GitHub Issue",
            "https://github.com/1",
            "github",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        assert!(title.contains("\u{1F419}"));
    }

    #[tokio::test]
    async fn test_notify_start_jira_source_uses_ticket_emoji() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "JIRA-99", "Jira Ticket", "https://jira.com/1", "jira");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        assert!(title.contains("\u{1F3AB}"));
    }

    #[tokio::test]
    async fn test_notify_start_unknown_source_uses_pushpin_emoji() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "X-1", "Unknown", "https://example.com", "custom");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        assert!(title.contains("\u{1F4CC}"));
    }

    #[tokio::test]
    async fn test_notify_start_embed_has_priority_and_status_fields() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let priority_field = fields.iter().find(|f| f["name"] == "Priority").unwrap();
        assert_eq!(priority_field["value"], "none");
        let status_field = fields.iter().find(|f| f["name"] == "Status").unwrap();
        assert_eq!(status_field["value"], "open");
    }

    #[tokio::test]
    async fn test_notify_start_blue_color() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert_eq!(body["embeds"][0]["color"], 0x3498db);
    }

    #[tokio::test]
    async fn test_notify_start_no_mention_when_no_user_id() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["content"].is_null());
    }

    #[tokio::test]
    async fn test_notify_success_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier
            .notify_success(&issue, "https://github.com/pr/1")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
        assert_eq!(content, "<@987654321>");
    }

    #[tokio::test]
    async fn test_notify_success_embed_fields_contain_source_and_issue_link() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "LIN-5",
            "Fix bug",
            "https://linear.app/issue/5",
            "linear",
        );

        notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/99")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();

        let source_field = fields.iter().find(|f| f["name"] == "Source").unwrap();
        assert!(source_field["value"].as_str().unwrap().contains("linear"));

        let issue_field = fields.iter().find(|f| f["name"] == "Issue").unwrap();
        let issue_val = issue_field["value"].as_str().unwrap();
        assert!(issue_val.contains("LIN-5"));
        assert!(issue_val.contains("https://linear.app/issue/5"));

        let pr_field = fields.iter().find(|f| f["name"] == "PR Link").unwrap();
        let pr_val = pr_field["value"].as_str().unwrap();
        assert!(pr_val.contains("https://github.com/org/repo/pull/99"));
    }

    #[tokio::test]
    async fn test_notify_success_no_content_when_no_user() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier
            .notify_success(&issue, "https://github.com/pr/1")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["content"].is_null());
    }

    #[tokio::test]
    async fn test_notify_completed_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_completed(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
        assert_eq!(content, "<@987654321>");
    }

    #[tokio::test]
    async fn test_notify_completed_has_note_field() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_completed(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let note_field = fields.iter().find(|f| f["name"] == "Note").unwrap();
        assert!(note_field["value"]
            .as_str()
            .unwrap()
            .contains("no PR URL was captured"));
    }

    #[tokio::test]
    async fn test_notify_completed_has_source_field_with_emoji() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "sentry");

        notifier.notify_completed(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let source_field = fields.iter().find(|f| f["name"] == "Source").unwrap();
        let val = source_field["value"].as_str().unwrap();
        assert!(val.contains("\u{1F534}"));
        assert!(val.contains("sentry"));
    }

    #[tokio::test]
    async fn test_notify_failed_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier.notify_failed(&issue, "oops").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
        assert_eq!(content, "<@987654321>");
    }

    #[tokio::test]
    async fn test_notify_failed_has_source_field() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "github");

        notifier.notify_failed(&issue, "error").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let source_field = fields.iter().find(|f| f["name"] == "Source").unwrap();
        let val = source_field["value"].as_str().unwrap();
        assert!(val.contains("\u{1F419}"));
        assert!(val.contains("github"));
    }

    #[tokio::test]
    async fn test_notify_status_no_content_field() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("All clear").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["content"].is_null());
    }

    #[tokio::test]
    async fn test_notify_status_truncates_long_message() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let long_msg = "z".repeat(3000);

        notifier.notify_status(&long_msg).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let desc = body["embeds"][0]["description"].as_str().unwrap();
        assert!(desc.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(desc.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_status_purple_color() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("test").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert_eq!(body["embeds"][0]["color"], 0x9b59b6);
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_mixed_sources() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![
            Issue::new("1", "LIN-1", "Linear Bug", "https://linear.app/1", "linear"),
            Issue::new(
                "2",
                "SEN-2",
                "Sentry Error",
                "https://sentry.io/2",
                "sentry",
            ),
            Issue::new(
                "3",
                "GH-3",
                "GitHub Issue",
                "https://github.com/3",
                "github",
            ),
        ];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 3);

        let field0_name = fields[0]["name"].as_str().unwrap();
        assert!(field0_name.contains("\u{1F4CB}")); // linear clipboard
        let field1_name = fields[1]["name"].as_str().unwrap();
        assert!(field1_name.contains("\u{1F534}")); // sentry red circle
        let field2_name = fields[2]["name"].as_str().unwrap();
        assert!(field2_name.contains("\u{1F419}")); // github octopus
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_description_text() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "P-1",
            "Test",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let desc = body["embeds"][0]["description"].as_str().unwrap();
        assert!(desc.contains("require attention"));
    }

    #[tokio::test]
    async fn test_send_boundary_status_199_is_error() {
        let mock = MockDiscordWebhookClient::new(199, "Not OK");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        let result = notifier.notify_status("test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_boundary_status_200_is_success() {
        let mock = MockDiscordWebhookClient::new(200, "OK");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        let result = notifier.notify_status("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_boundary_status_299_is_success() {
        let mock = MockDiscordWebhookClient::new(299, "OK");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        let result = notifier.notify_status("test").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_boundary_status_300_is_error() {
        let mock = MockDiscordWebhookClient::new(300, "Redirect");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        let result = notifier.notify_status("test").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_ask_question_without_mention_no_user() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-1", "Which branch?", None, vec![], None, None);

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(!content.contains("<@"));
        assert!(content.contains("[CLAUDEAR-Q:tok-1]"));
        assert!(content.contains("Which branch?"));
    }

    #[tokio::test]
    async fn test_ask_question_with_user_mention() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-2", "Pick env?", None, vec![], None, None);

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.starts_with("<@987654321>"));
    }

    #[tokio::test]
    async fn test_ask_question_with_why_field() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request(
            "tok-3",
            "Which DB?",
            None,
            vec![],
            Some("Multiple databases found"),
            None,
        );

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("Why: Multiple databases found"));
    }

    #[tokio::test]
    async fn test_ask_question_with_context_field() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request(
            "tok-4",
            "Which target?",
            Some("The repo has multiple deploy targets"),
            vec![],
            None,
            None,
        );

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("Context: The repo has multiple deploy targets"));
    }

    #[tokio::test]
    async fn test_ask_question_truncates_long_context() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let long_context = "x".repeat(600);
        let request = make_ask_request(
            "tok-5",
            "Question?",
            Some(&long_context),
            vec![],
            None,
            None,
        );

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("Context:"));
        assert!(content.contains("..."));
    }

    #[tokio::test]
    async fn test_ask_question_with_options() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request(
            "tok-6",
            "Pick one",
            None,
            vec!["alpha", "beta", "gamma"],
            None,
            None,
        );

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("Options: alpha | beta | gamma"));
    }

    #[tokio::test]
    async fn test_ask_question_empty_options_not_shown() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-7", "Free text answer?", None, vec![], None, None);

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(!content.contains("Options:"));
    }

    #[tokio::test]
    async fn test_ask_question_includes_reply_instruction() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-8", "Confirm?", None, vec![], None, None);

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("Reply to this message in Discord with your answer."));
    }

    #[tokio::test]
    async fn test_ask_question_disabled_webhook_returns_ok_delivery() {
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
            ..Default::default()
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-9", "Confirm?", None, vec![], None, None);

        let result = notifier.ask_question(&issue, &request).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().channel, "discord");
    }

    #[tokio::test]
    async fn test_poll_question_replies_no_bot_token_returns_empty() {
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
            bot_token: None,
            channel_id: Some("channel-123".to_string()),
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let request = make_ask_request("tok-10", "Question?", None, vec![], None, None);

        let replies = notifier
            .poll_question_replies(&request, chrono::Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_empty_bot_token_returns_empty() {
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
            bot_token: Some("".to_string()),
            channel_id: Some("channel-123".to_string()),
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let request = make_ask_request("tok-11", "Question?", None, vec![], None, None);

        let replies = notifier
            .poll_question_replies(&request, chrono::Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_no_channel_id_returns_empty() {
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
            bot_token: Some("valid-token".to_string()),
            channel_id: None,
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let request = make_ask_request("tok-12", "Question?", None, vec![], None, None);

        let replies = notifier
            .poll_question_replies(&request, chrono::Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_empty_channel_id_returns_empty() {
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: None,
            bot_token: Some("valid-token".to_string()),
            channel_id: Some("".to_string()),
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let request = make_ask_request("tok-13", "Question?", None, vec![], None, None);

        let replies = notifier
            .poll_question_replies(&request, chrono::Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[test]
    fn test_get_target_discord_id_for_issue_with_resolved_user() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "alice".to_string(),
            crate::config::UserConfig {
                discord_id: Some("alice-discord-id".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("global-id".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(
            config,
            MockDiscordWebhookClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "alice");

        assert_eq!(
            notifier.get_target_discord_id_for_issue(&issue),
            Some("alice-discord-id".to_string())
        );
    }

    #[test]
    fn test_get_target_discord_id_for_issue_fallback_to_global() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("global-id".to_string()),
            ..Default::default()
        };
        let notifier =
            DiscordNotifier::with_http_client(config, MockDiscordWebhookClient::success());
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        assert_eq!(
            notifier.get_target_discord_id_for_issue(&issue),
            Some("global-id".to_string())
        );
    }

    #[test]
    fn test_get_target_discord_id_for_issue_resolved_user_no_discord() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "bob".to_string(),
            crate::config::UserConfig {
                discord_id: None,
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: Some("global-id".to_string()),
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client_and_registry(
            config,
            MockDiscordWebhookClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "bob");

        assert_eq!(
            notifier.get_target_discord_id_for_issue(&issue),
            Some("global-id".to_string())
        );
    }

    #[test]
    fn test_get_target_discord_id_for_issue_no_user_at_all() {
        let config = DiscordConfig {
            webhook_url: Some("https://example.com".to_string()),
            user_id: None,
            ..Default::default()
        };
        let notifier =
            DiscordNotifier::with_http_client(config, MockDiscordWebhookClient::success());
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        assert_eq!(notifier.get_target_discord_id_for_issue(&issue), None);
    }

    #[tokio::test]
    async fn test_notify_start_truncates_long_short_id() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let long_id = "X".repeat(200);
        let issue = Issue::new("1", &long_id, "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let title = body["embeds"][0]["title"].as_str().unwrap();
        assert!(title.len() < 200);
    }

    #[tokio::test]
    async fn test_notify_start_truncates_long_description() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let long_title = "D".repeat(3000);
        let issue = Issue::new("1", "P-1", &long_title, "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let desc = body["embeds"][0]["description"].as_str().unwrap();
        assert!(desc.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(desc.ends_with("..."));
    }

    #[tokio::test]
    async fn test_ask_question_with_all_fields() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config_with_user(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request(
            "tok-all",
            "Select deployment target",
            Some("Found staging and prod"),
            vec!["staging", "production"],
            Some("Need to know before PR"),
            None,
        );

        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let content = body["content"].as_str().unwrap();
        assert!(content.contains("<@987654321>"));
        assert!(content.contains("[CLAUDEAR-Q:tok-all]"));
        assert!(content.contains("Select deployment target"));
        assert!(content.contains("Why: Need to know before PR"));
        assert!(content.contains("Context: Found staging and prod"));
        assert!(content.contains("Options: staging | production"));
        assert!(content.contains("Reply to this message"));
        assert_eq!(delivery.channel, "discord");
    }

    #[tokio::test]
    async fn test_notify_success_truncates_long_pr_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");
        let long_pr_url = format!("https://github.com/{}", "a".repeat(2500));

        notifier.notify_success(&issue, &long_pr_url).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let url = body["embeds"][0]["url"].as_str().unwrap();
        assert!(url.len() <= MAX_URL_LENGTH);
    }

    #[tokio::test]
    async fn test_notify_failed_short_error_not_truncated() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        notifier
            .notify_failed(&issue, "Compilation error on line 42")
            .await
            .unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let error_field = fields.iter().find(|f| f["name"] == "Error").unwrap();
        assert_eq!(
            error_field["value"].as_str().unwrap(),
            "Compilation error on line 42"
        );
    }

    #[test]
    fn test_with_http_client_and_registry_creates_valid_notifier() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "test".to_string(),
            crate::config::UserConfig {
                discord_id: Some("test-discord".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let mock = MockDiscordWebhookClient::success();
        let notifier =
            DiscordNotifier::with_http_client_and_registry(enabled_config(), mock, registry);

        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "discord");
    }

    #[tokio::test]
    async fn test_send_no_webhook_url_returns_ok_without_calling_http() {
        let mock = MockDiscordWebhookClient::success();
        let config = DiscordConfig {
            webhook_url: None,
            user_id: None,
            ..Default::default()
        };
        let notifier = DiscordNotifier::with_http_client(config, mock);
        let issue = Issue::new("1", "P-1", "Test", "https://example.com", "linear");

        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 0);
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_fields_have_inline_true() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "P-1",
            "Test",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        for field in fields {
            assert_eq!(field["inline"], true);
        }
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_field_value_is_markdown_link() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "P-1",
            "Fix memory leak",
            "https://example.com/issue/1",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let fields = body["embeds"][0]["fields"].as_array().unwrap();
        let value = fields[0]["value"].as_str().unwrap();
        assert!(value.starts_with('['));
        assert!(value.contains("]("));
        assert!(value.contains("https://example.com/issue/1"));
    }

    #[tokio::test]
    async fn test_ask_question_target_from_resolved_user_registry() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "charlie".to_string(),
            crate::config::UserConfig {
                discord_id: Some("charlie-discord".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let config = DiscordConfig {
            webhook_url: Some("https://discord.com/api/webhooks/123/abc".to_string()),
            user_id: Some("fallback-id".to_string()),
            ..Default::default()
        };
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "charlie");
        let request = make_ask_request("tok-resolved", "Confirm?", None, vec![], None, None);

        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("charlie-discord"));
    }

    #[tokio::test]
    async fn test_ask_question_http_error_propagates() {
        let mock = MockDiscordWebhookClient::error(500, "Server Error");
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-err", "Confirm?", None, vec![], None, None);

        let result = notifier.ask_question(&issue, &request).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate_string_at_char_boundary_with_multibyte() {
        let s = "abcdefghij\u{00E9}klm"; // e-acute is 2 bytes
        let result = truncate_string(s, 12);
        assert!(result.len() <= 12);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_string_emoji_boundary() {
        let s = "hello \u{1F600} world"; // grinning face is 4 bytes
        let result = truncate_string(s, 10);
        assert!(result.len() <= 10);
    }

    #[tokio::test]
    async fn test_notify_start_embed_url_matches_issue_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "P-1",
            "Test",
            "https://linear.app/team/issue/P-1",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let url = body["embeds"][0]["url"].as_str().unwrap();
        assert_eq!(url, "https://linear.app/team/issue/P-1");
    }

    #[tokio::test]
    async fn test_notify_completed_embed_url_matches_issue_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "P-1",
            "Test",
            "https://linear.app/team/issue/P-1",
            "linear",
        );

        notifier.notify_completed(&issue).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let url = body["embeds"][0]["url"].as_str().unwrap();
        assert_eq!(url, "https://linear.app/team/issue/P-1");
    }

    #[tokio::test]
    async fn test_notify_failed_embed_url_matches_issue_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "P-1",
            "Test",
            "https://linear.app/team/issue/P-1",
            "linear",
        );

        notifier.notify_failed(&issue, "err").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        let url = body["embeds"][0]["url"].as_str().unwrap();
        assert_eq!(url, "https://linear.app/team/issue/P-1");
    }

    #[tokio::test]
    async fn test_notify_status_has_no_title() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("All good").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"][0]["title"].is_null());
    }

    #[tokio::test]
    async fn test_notify_status_has_no_url() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("All good").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"][0]["url"].is_null());
    }

    #[tokio::test]
    async fn test_notify_status_has_no_fields() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("All good").await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"][0]["fields"].is_null());
    }

    #[tokio::test]
    async fn test_ask_question_embeds_is_null() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = make_ask_request("tok-no-embed", "Confirm?", None, vec![], None, None);

        notifier.ask_question(&issue, &request).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"].is_null());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_orange_color() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "P-1",
            "Test",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert_eq!(body["embeds"][0]["color"], 0xf39c12);
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_no_url_in_embed() {
        let mock = MockDiscordWebhookClient::success();
        let notifier = DiscordNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "P-1",
            "Test",
            "https://example.com",
            "linear",
        )];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let (_, body) = notifier.http.get_last_call().unwrap();
        assert!(body["embeds"][0]["url"].is_null());
    }

    // --- Synchronous tests for standalone build_* helpers ---

    fn test_issue() -> Issue {
        Issue::new(
            "42",
            "PROJ-42",
            "Fix the widget",
            "https://example.com/issue/42",
            "linear",
        )
    }

    #[test]
    fn test_build_start_message_with_mention() {
        let issue = test_issue();
        let msg = build_start_message(&issue, Some("<@12345>".to_string()));

        assert_eq!(msg.content.as_deref(), Some("<@12345>"));
        let embeds = msg.embeds.as_ref().unwrap();
        assert_eq!(embeds.len(), 1);
        let embed = &embeds[0];
        assert!(embed
            .title
            .as_ref()
            .unwrap()
            .contains("Processing: PROJ-42"));
        assert_eq!(embed.description.as_deref(), Some("Fix the widget"));
        assert_eq!(embed.url.as_deref(), Some("https://example.com/issue/42"));
        assert_eq!(embed.color, Some(0x3498db));
        let fields = embed.fields.as_ref().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "Source");
        assert_eq!(fields[0].value, "linear");
        assert_eq!(fields[1].name, "Priority");
        assert_eq!(fields[2].name, "Status");
        assert_eq!(embed.footer.as_ref().unwrap().text, "Claudear");
        assert!(embed.timestamp.is_some());
    }

    #[test]
    fn test_build_start_message_without_mention() {
        let issue = test_issue();
        let msg = build_start_message(&issue, None);

        assert!(msg.content.is_none());
        assert!(msg.embeds.is_some());
    }

    #[test]
    fn test_build_success_message_fields() {
        let issue = test_issue();
        let msg = build_success_message(
            &issue,
            "https://github.com/org/repo/pull/99",
            Some("<@user>".to_string()),
        );

        assert_eq!(msg.content.as_deref(), Some("<@user>"));
        let embed = &msg.embeds.as_ref().unwrap()[0];
        assert!(embed
            .title
            .as_ref()
            .unwrap()
            .contains("PR Created: PROJ-42"));
        assert_eq!(embed.color, Some(0x2ecc71));
        assert_eq!(
            embed.url.as_deref(),
            Some("https://github.com/org/repo/pull/99")
        );
        let fields = embed.fields.as_ref().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "Source");
        assert!(fields[0].value.contains("linear"));
        assert_eq!(fields[1].name, "Issue");
        assert!(fields[1].value.contains("PROJ-42"));
        assert_eq!(fields[2].name, "PR Link");
        assert!(fields[2]
            .value
            .contains("https://github.com/org/repo/pull/99"));
    }

    #[test]
    fn test_build_success_message_without_mention() {
        let issue = test_issue();
        let msg = build_success_message(&issue, "https://pr.url", None);

        assert!(msg.content.is_none());
    }

    #[test]
    fn test_build_completed_message_fields() {
        let issue = test_issue();
        let msg = build_completed_message(&issue, Some("<@u>".to_string()));

        assert_eq!(msg.content.as_deref(), Some("<@u>"));
        let embed = &msg.embeds.as_ref().unwrap()[0];
        assert!(embed.title.as_ref().unwrap().contains("Completed: PROJ-42"));
        assert_eq!(embed.color, Some(0x9b59b6));
        let fields = embed.fields.as_ref().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "Source");
        assert_eq!(fields[1].name, "Note");
        assert!(fields[1].value.contains("no PR URL was captured"));
    }

    #[test]
    fn test_build_completed_message_without_mention() {
        let issue = test_issue();
        let msg = build_completed_message(&issue, None);

        assert!(msg.content.is_none());
    }

    #[test]
    fn test_build_failed_message_fields() {
        let issue = test_issue();
        let msg = build_failed_message(
            &issue,
            "Build failed with exit code 1",
            Some("<@u>".to_string()),
        );

        assert_eq!(msg.content.as_deref(), Some("<@u>"));
        let embed = &msg.embeds.as_ref().unwrap()[0];
        assert!(embed.title.as_ref().unwrap().contains("Failed: PROJ-42"));
        assert_eq!(embed.color, Some(0xe74c3c));
        let fields = embed.fields.as_ref().unwrap();
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "Source");
        assert_eq!(fields[1].name, "Error");
        assert_eq!(fields[1].value, "Build failed with exit code 1");
    }

    #[test]
    fn test_build_failed_message_truncates_long_error() {
        let issue = test_issue();
        let long_error = "x".repeat(2000);
        let msg = build_failed_message(&issue, &long_error, None);

        let fields = msg.embeds.as_ref().unwrap()[0].fields.as_ref().unwrap();
        let error_value = &fields[1].value;
        assert!(error_value.len() <= 1003);
        assert!(error_value.ends_with("..."));
    }

    #[test]
    fn test_build_status_message_fields() {
        let msg = build_status_message("System is healthy");

        assert!(msg.content.is_none());
        let embed = &msg.embeds.as_ref().unwrap()[0];
        assert!(embed.title.is_none());
        assert_eq!(embed.description.as_deref(), Some("System is healthy"));
        assert!(embed.url.is_none());
        assert_eq!(embed.color, Some(0x9b59b6));
        assert!(embed.fields.is_none());
        assert_eq!(embed.footer.as_ref().unwrap().text, "Claudear");
    }

    #[test]
    fn test_build_status_message_truncates_long_text() {
        let long_msg = "z".repeat(3000);
        let msg = build_status_message(&long_msg);

        let desc = msg.embeds.as_ref().unwrap()[0]
            .description
            .as_ref()
            .unwrap();
        assert!(desc.len() <= MAX_DESCRIPTION_LENGTH);
        assert!(desc.ends_with("..."));
    }

    #[test]
    fn test_build_urgent_issues_message_empty_returns_none() {
        let result = build_urgent_issues_message(&[], None);
        assert!(result.is_none());
    }

    #[test]
    fn test_build_urgent_issues_message_single() {
        let issues = vec![test_issue()];
        let msg = build_urgent_issues_message(&issues, Some("<@u>".to_string())).unwrap();

        assert_eq!(msg.content.as_deref(), Some("<@u>"));
        let embed = &msg.embeds.as_ref().unwrap()[0];
        assert!(embed
            .title
            .as_ref()
            .unwrap()
            .contains("1 Urgent Issue Detected"));
        assert!(!embed.title.as_ref().unwrap().contains("Issues"));
        assert_eq!(embed.color, Some(0xf39c12));
        let fields = embed.fields.as_ref().unwrap();
        assert_eq!(fields.len(), 1);
        assert!(fields[0].name.contains("PROJ-42"));
    }

    #[test]
    fn test_build_urgent_issues_message_plural() {
        let issues = vec![
            Issue::new("1", "P-1", "Issue 1", "https://example.com/1", "linear"),
            Issue::new("2", "P-2", "Issue 2", "https://example.com/2", "sentry"),
        ];
        let msg = build_urgent_issues_message(&issues, None).unwrap();

        assert!(msg.content.is_none());
        let title = msg.embeds.as_ref().unwrap()[0].title.as_ref().unwrap();
        assert!(title.contains("2 Urgent Issues Detected"));
    }

    #[test]
    fn test_build_urgent_issues_message_limits_to_ten() {
        let issues: Vec<Issue> = (1..=15)
            .map(|i| {
                Issue::new(
                    i.to_string(),
                    format!("P-{}", i),
                    format!("Issue {}", i),
                    format!("https://example.com/{}", i),
                    "linear",
                )
            })
            .collect();
        let msg = build_urgent_issues_message(&issues, None).unwrap();

        let fields = msg.embeds.as_ref().unwrap()[0].fields.as_ref().unwrap();
        assert_eq!(fields.len(), 10);
    }

    #[test]
    fn test_build_ask_question_message_minimal() {
        let issue = test_issue();
        let request = make_ask_request("tok-1", "Which branch?", None, vec![], None, None);
        let msg = build_ask_question_message(&issue, &request, None);

        assert!(msg.embeds.is_none());
        let content = msg.content.as_ref().unwrap();
        assert!(content.contains("[CLAUDEAR-Q:tok-1]"));
        assert!(content.contains("Human input needed for PROJ-42"));
        assert!(content.contains("Which branch?"));
        assert!(content.contains("Reply to this message in Discord with your answer."));
        assert!(!content.contains("Why:"));
        assert!(!content.contains("Context:"));
        assert!(!content.contains("Options:"));
    }

    #[test]
    fn test_build_ask_question_message_with_all_fields() {
        let issue = test_issue();
        let request = make_ask_request(
            "tok-all",
            "Select target",
            Some("Found staging and prod"),
            vec!["staging", "production"],
            Some("Need to know before PR"),
            None,
        );
        let msg = build_ask_question_message(&issue, &request, Some("<@987654321>".to_string()));

        let content = msg.content.as_ref().unwrap();
        assert!(content.starts_with("<@987654321>"));
        assert!(content.contains("[CLAUDEAR-Q:tok-all]"));
        assert!(content.contains("Select target"));
        assert!(content.contains("Why: Need to know before PR"));
        assert!(content.contains("Context: Found staging and prod"));
        assert!(content.contains("Options: staging | production"));
    }

    #[test]
    fn test_build_ask_question_message_truncates_long_context() {
        let issue = test_issue();
        let long_context = "x".repeat(600);
        let request = make_ask_request(
            "tok-ctx",
            "Question?",
            Some(&long_context),
            vec![],
            None,
            None,
        );
        let msg = build_ask_question_message(&issue, &request, None);

        let content = msg.content.as_ref().unwrap();
        assert!(content.contains("Context:"));
        assert!(content.contains("..."));
    }
}

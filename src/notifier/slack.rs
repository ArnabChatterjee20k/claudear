//! Slack notifier with Block Kit formatting and Q&A support.

use super::get_source_emoji;
use super::Notifier;
use crate::config::SlackConfig;
use crate::error::{Error, Result};
use crate::http::HttpResponse;
use crate::reports::Report;
use crate::types::{AskDelivery, AskReply, AskRequest, Issue};
use crate::users::UserRegistry;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// HTTP abstraction
// ---------------------------------------------------------------------------

/// Trait for HTTP client used by Slack notifier.
#[async_trait]
pub trait SlackHttpClient: Send + Sync {
    /// POST JSON to `url`. If `auth_token` is `Some`, include `Authorization: Bearer {token}`.
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_token: Option<&str>,
    ) -> Result<HttpResponse>;

    /// GET JSON from `url`. If `auth_token` is `Some`, include `Authorization: Bearer {token}`.
    async fn get_json(&self, url: &str, auth_token: Option<&str>) -> Result<HttpResponse>;
}

/// Real HTTP client using reqwest.
pub struct ReqwestSlackHttpClient {
    client: reqwest::Client,
}

impl ReqwestSlackHttpClient {
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

impl Default for ReqwestSlackHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SlackHttpClient for ReqwestSlackHttpClient {
    async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        auth_token: Option<&str>,
    ) -> Result<HttpResponse> {
        let mut req = self.client.post(url).json(body);
        if let Some(token) = auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let response = req.send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }

    async fn get_json(&self, url: &str, auth_token: Option<&str>) -> Result<HttpResponse> {
        let mut req = self.client.get(url);
        if let Some(token) = auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        let response = req.send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }
}

// ---------------------------------------------------------------------------
// Block Kit types
// ---------------------------------------------------------------------------

/// A Slack Block Kit message payload.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SlackMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) channel: Option<String>,
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) blocks: Option<Vec<SlackBlock>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thread_ts: Option<String>,
}

/// A single block in Block Kit.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub(crate) enum SlackBlock {
    #[serde(rename = "header")]
    Header { text: SlackText },
    #[serde(rename = "section")]
    Section {
        text: SlackText,
        #[serde(skip_serializing_if = "Option::is_none")]
        fields: Option<Vec<SlackText>>,
    },
    #[serde(rename = "context")]
    Context { elements: Vec<SlackText> },
    #[allow(dead_code)]
    #[serde(rename = "divider")]
    Divider,
}

/// A Block Kit text object.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SlackText {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) text: String,
}

impl SlackText {
    pub(crate) fn mrkdwn(text: impl Into<String>) -> Self {
        Self {
            kind: "mrkdwn".to_string(),
            text: text.into(),
        }
    }

    pub(crate) fn plain_text(text: impl Into<String>) -> Self {
        Self {
            kind: "plain_text".to_string(),
            text: text.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Slack API response types
// ---------------------------------------------------------------------------

/// Response from Slack Web API methods.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SlackApiResponse {
    pub(crate) ok: bool,
    #[serde(default)]
    pub(crate) error: Option<String>,
    /// Message timestamp (returned by chat.postMessage).
    #[serde(default)]
    pub(crate) ts: Option<String>,
    /// Messages returned by conversations.history / conversations.replies.
    #[serde(default)]
    pub(crate) messages: Option<Vec<SlackApiMessage>>,
}

/// A message object from Slack API.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SlackApiMessage {
    pub(crate) ts: String,
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) user: Option<String>,
    #[serde(default)]
    pub(crate) bot_id: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) thread_ts: Option<String>,
}

// ---------------------------------------------------------------------------
// Constants & helpers
// ---------------------------------------------------------------------------

const SLACK_API_BASE: &str = "https://slack.com/api/";

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

/// Convert a Slack message timestamp (e.g. `"1709123456.789012"`) to `DateTime<Utc>`.
pub(crate) fn slack_ts_to_datetime(ts: &str) -> Option<DateTime<Utc>> {
    let secs_f64: f64 = ts.parse().ok()?;
    let secs = secs_f64.trunc() as i64;
    let nanos = (secs_f64.fract().abs() * 1_000_000_000.0).min(999_999_999.0) as u32;
    DateTime::from_timestamp(secs, nanos)
}

// ---------------------------------------------------------------------------
// SlackNotifier
// ---------------------------------------------------------------------------

/// Slack notifier supporting both Incoming Webhooks and Bot Token API.
pub struct SlackNotifier<H: SlackHttpClient = ReqwestSlackHttpClient> {
    config: SlackConfig,
    http: H,
    user_registry: UserRegistry,
}

impl SlackNotifier<ReqwestSlackHttpClient> {
    pub fn new(config: SlackConfig, user_registry: UserRegistry) -> Self {
        Self {
            config,
            http: ReqwestSlackHttpClient::new(),
            user_registry,
        }
    }
}

impl<H: SlackHttpClient> SlackNotifier<H> {
    /// Create a new Slack notifier with a custom HTTP client.
    pub fn with_http_client(config: SlackConfig, http: H) -> Self {
        Self {
            config,
            http,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
        }
    }

    /// Create a new Slack notifier with a custom HTTP client and user registry.
    pub fn with_http_client_and_registry(
        config: SlackConfig,
        http: H,
        user_registry: UserRegistry,
    ) -> Self {
        Self {
            config,
            http,
            user_registry,
        }
    }

    // -----------------------------------------------------------------------
    // Send helpers
    // -----------------------------------------------------------------------

    /// Send a notification message.
    ///
    /// Tries the webhook URL first (no channel required). Falls back to
    /// `chat.postMessage` with bot_token + channel_id.
    async fn send(&self, message: SlackMessage) -> Result<()> {
        // Webhook path
        if let Some(ref webhook_url) = self.config.webhook_url {
            let body = serde_json::json!({
                "text": message.text,
                "blocks": message.blocks,
            });
            let response = self.http.post_json(webhook_url, &body, None).await?;

            if response.status < 200 || response.status >= 300 {
                return Err(Error::notifier(
                    "slack",
                    format!("Webhook error: {}", response.body),
                ));
            }
            return Ok(());
        }

        // Bot API path
        if let (Some(ref token), Some(ref channel_id)) =
            (&self.config.bot_token, &self.config.channel_id)
        {
            if !token.is_empty() && !channel_id.is_empty() {
                self.post_chat_message(token, channel_id, &message).await?;
                return Ok(());
            }
        }

        tracing::warn!("Slack send() called but no webhook_url or bot_token+channel_id configured, message dropped");
        Ok(())
    }

    /// Send a message via `chat.postMessage` and return the message `ts`.
    ///
    /// Used by `ask_question` and `send_to_channel` where we need the
    /// timestamp for threading.
    async fn send_to_channel(&self, message: SlackMessage) -> Result<Option<String>> {
        let token = match self.config.bot_token.as_deref() {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(None),
        };
        let channel_id = match self.config.channel_id.as_deref() {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(None),
        };

        let ts = self.post_chat_message(token, channel_id, &message).await?;
        Ok(Some(ts))
    }

    /// POST to `chat.postMessage` and return the resulting `ts`.
    async fn post_chat_message(
        &self,
        token: &str,
        channel_id: &str,
        message: &SlackMessage,
    ) -> Result<String> {
        let url = format!("{}chat.postMessage", SLACK_API_BASE);
        let body = serde_json::json!({
            "channel": channel_id,
            "text": message.text,
            "blocks": message.blocks,
            "thread_ts": message.thread_ts,
        });
        let response = self.http.post_json(&url, &body, Some(token)).await?;

        if response.status < 200 || response.status >= 300 {
            return Err(Error::notifier(
                "slack",
                format!("chat.postMessage HTTP error: {}", response.body),
            ));
        }

        let api_resp: SlackApiResponse = serde_json::from_str(&response.body).map_err(|e| {
            Error::notifier("slack", format!("Failed to parse API response: {}", e))
        })?;

        if !api_resp.ok {
            return Err(Error::notifier(
                "slack",
                format!(
                    "chat.postMessage error: {}",
                    api_resp.error.unwrap_or_else(|| "unknown".to_string())
                ),
            ));
        }

        Ok(api_resp.ts.unwrap_or_default())
    }

    // -----------------------------------------------------------------------
    // Bot channel check
    // -----------------------------------------------------------------------

    fn has_bot_channel(&self) -> bool {
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

    // -----------------------------------------------------------------------
    // User mention helpers
    // -----------------------------------------------------------------------

    fn get_user_mention(&self) -> Option<String> {
        self.config.user_id.as_ref().map(|id| format!("<@{}>", id))
    }

    fn get_user_mention_for_issue(&self, issue: &Issue) -> Option<String> {
        // Check for resolved user first
        if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
            if let Some(user) = self.user_registry.get_by_slug(&slug) {
                if let Some(ref slack_id) = user.slack_id {
                    return Some(format!("<@{}>", slack_id));
                }
            }
        }
        // Fall back to global config
        self.config.user_id.as_ref().map(|id| format!("<@{}>", id))
    }

    fn get_target_slack_id_for_issue(&self, issue: &Issue) -> Option<String> {
        if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
            if let Some(user) = self.user_registry.get_by_slug(&slug) {
                if let Some(ref slack_id) = user.slack_id {
                    return Some(slack_id.clone());
                }
            }
        }
        self.config.user_id.clone()
    }

    fn expected_reply_user_id(&self, request: &AskRequest) -> Option<String> {
        request
            .target_slack_id
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

// ---------------------------------------------------------------------------
// Message builders
// ---------------------------------------------------------------------------

/// Return the current UTC timestamp in RFC 3339 format.
pub(crate) fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Build the Slack message for a "processing started" notification.
pub(crate) fn build_start_message(issue: &Issue, mention: Option<String>) -> SlackMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    let mut fallback = format!("{} Processing: {} - {}", emoji, short_id, title);
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(format!("{} Processing: {}", emoji, short_id)),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*<{}|{}>*", url, title)),
            fields: Some(vec![
                SlackText::mrkdwn(format!("*Source:* {}", source)),
                SlackText::mrkdwn(format!("*Priority:* {}", issue.priority)),
                SlackText::mrkdwn(format!("*Status:* {}", issue.status)),
            ]),
        },
    ];
    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    }
}

/// Build the Slack message for a "PR created" notification.
pub(crate) fn build_success_message(
    issue: &Issue,
    pr_url: &str,
    mention: Option<String>,
) -> SlackMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let issue_url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let pr_url_truncated = truncate_string(pr_url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    let mut fallback = format!("\u{2705} PR Created: {} - {}", short_id, pr_url_truncated);
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(format!("\u{2705} PR Created: {}", short_id)),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*<{}|{}>*", pr_url_truncated, title)),
            fields: Some(vec![
                SlackText::mrkdwn(format!("*Source:* {} {}", emoji, source)),
                SlackText::mrkdwn(format!("*Issue:* <{}|{}>", issue_url, short_id)),
            ]),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*PR Link:* <{}|View PR>", pr_url_truncated)),
            fields: None,
        },
    ];
    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    }
}

/// Build the Slack message for a "completed without PR" notification.
pub(crate) fn build_completed_message(issue: &Issue, mention: Option<String>) -> SlackMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    let mut fallback = format!("\u{2714}\u{FE0F} Completed: {} - {}", short_id, title);
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(format!("\u{2714}\u{FE0F} Completed: {}", short_id)),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*<{}|{}>*", url, title)),
            fields: Some(vec![
                SlackText::mrkdwn(format!("*Source:* {} {}", emoji, source)),
                SlackText::mrkdwn(
                    "*Note:* Claude completed but no PR URL was captured".to_string(),
                ),
            ]),
        },
    ];
    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    }
}

/// Build the Slack message for a "failed" notification.
pub(crate) fn build_failed_message(
    issue: &Issue,
    error: &str,
    mention: Option<String>,
) -> SlackMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let title = truncate_string(&issue.title, MAX_DESCRIPTION_LENGTH);
    let url = truncate_string(&issue.url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);
    let error_display = truncate_string(error, 1000);

    let mut fallback = format!("\u{274C} Failed: {} - {}", short_id, error_display);
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(format!("\u{274C} Failed: {}", short_id)),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*<{}|{}>*", url, title)),
            fields: Some(vec![SlackText::mrkdwn(format!(
                "*Source:* {} {}",
                emoji, source
            ))]),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!("*Error:*\n```{}```", error_display)),
            fields: None,
        },
    ];
    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    }
}

/// Build the Slack message for a status notification.
pub(crate) fn build_status_message(message: &str) -> SlackMessage {
    let message_truncated = truncate_string(message, MAX_DESCRIPTION_LENGTH);

    SlackMessage {
        channel: None,
        text: message_truncated.clone(),
        blocks: Some(vec![
            SlackBlock::Section {
                text: SlackText::mrkdwn(message_truncated),
                fields: None,
            },
            SlackBlock::Context {
                elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
            },
        ]),
        thread_ts: None,
    }
}

/// Build the Slack message for an "urgent issues" notification.
///
/// Returns `None` when the issue list is empty (nothing to send).
pub(crate) fn build_urgent_issues_message(
    issues: &[Issue],
    mention: Option<String>,
) -> Option<SlackMessage> {
    if issues.is_empty() {
        return None;
    }

    let fields: Vec<SlackText> = issues
        .iter()
        .take(10)
        .map(|issue| {
            let emoji = get_source_emoji(&issue.source);
            let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
            let title = truncate_string(&issue.title, 50);
            let url = truncate_string(&issue.url, MAX_URL_LENGTH);
            SlackText::mrkdwn(format!("{} <{}|{} - {}>", emoji, url, short_id, title))
        })
        .collect();

    let header_text = format!(
        "\u{1F6A8} {} Urgent Issue{} Detected",
        issues.len(),
        if issues.len() > 1 { "s" } else { "" }
    );

    let mut fallback = header_text.clone();
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(header_text),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn("The following issues require attention:".to_string()),
            fields: None,
        },
    ];

    // Add each issue as its own section for readability
    for field in &fields {
        blocks.push(SlackBlock::Section {
            text: field.clone(),
            fields: None,
        });
    }

    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    Some(SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    })
}

/// Build the Slack message for a "merged" notification.
pub(crate) fn build_merged_message(
    issue: &Issue,
    pr_url: &str,
    mention: Option<String>,
) -> SlackMessage {
    let emoji = get_source_emoji(&issue.source);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);
    let pr_url_truncated = truncate_string(pr_url, MAX_URL_LENGTH);
    let source = truncate_string(&issue.source, MAX_SOURCE_LENGTH);

    let mut fallback = format!("\u{1F389} PR Merged: {} - {}", short_id, pr_url_truncated);
    if let Some(ref m) = mention {
        fallback = format!("{} {}", m, fallback);
    }

    let mut blocks = vec![
        SlackBlock::Header {
            text: SlackText::plain_text(format!(
                "\u{1F389} PR Merged & Issue Resolved: {}",
                short_id
            )),
        },
        SlackBlock::Section {
            text: SlackText::mrkdwn(format!(
                "*Source:* {} {} | *PR:* <{}|View PR>",
                emoji, source, pr_url_truncated
            )),
            fields: None,
        },
    ];
    if let Some(ref m) = mention {
        blocks.push(SlackBlock::Context {
            elements: vec![SlackText::mrkdwn(m.clone())],
        });
    }
    blocks.push(SlackBlock::Context {
        elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
    });

    SlackMessage {
        channel: None,
        text: fallback,
        blocks: Some(blocks),
        thread_ts: None,
    }
}

/// Build the Slack message for a scheduled report.
pub(crate) fn build_report_message(report: &Report) -> SlackMessage {
    let text = report.format_text();
    let truncated = truncate_string(&text, MAX_DESCRIPTION_LENGTH);

    SlackMessage {
        channel: None,
        text: truncated.clone(),
        blocks: Some(vec![
            SlackBlock::Header {
                text: SlackText::plain_text(format!(
                    "\u{1F4CA} Claudear Report: {}",
                    report.period
                )),
            },
            SlackBlock::Section {
                text: SlackText::mrkdwn(format!(
                    "*Issues:* {} attempted, {} succeeded, {} failed\n*PRs:* {} created, {} merged\n*Success Rate:* {:.1}%",
                    report.issues_attempted,
                    report.issues_succeeded,
                    report.issues_failed,
                    report.prs_created,
                    report.prs_merged,
                    report.success_rate,
                )),
                fields: None,
            },
            SlackBlock::Context {
                elements: vec![SlackText::mrkdwn(format!("Claudear | {}", timestamp()))],
            },
        ]),
        thread_ts: None,
    }
}

/// Build the Slack message for a human-in-the-loop question.
pub(crate) fn build_ask_question_message(
    issue: &Issue,
    request: &AskRequest,
    mention: Option<String>,
) -> SlackMessage {
    let token = format!("[CLAUDEAR-Q:{}]", request.correlation_id);
    let short_id = truncate_string(&issue.short_id, MAX_SHORT_ID_LENGTH);

    let mut text = String::new();
    if let Some(ref m) = mention {
        text.push_str(m);
        text.push(' ');
    }
    text.push_str(&format!(
        "{} Human input needed for {}:\n{}",
        token, short_id, request.question.question
    ));
    if let Some(ref why) = request.question.why {
        text.push_str(&format!("\nWhy: {}", why));
    }
    if let Some(ref ctx) = request.question.context {
        text.push_str(&format!("\nContext: {}", truncate_string(ctx, 400)));
    }
    if !request.question.options.is_empty() {
        text.push_str(&format!(
            "\nOptions: {}",
            request.question.options.join(" | ")
        ));
    }
    text.push_str("\nReply in this thread with your answer.");

    SlackMessage {
        channel: None,
        text: text.clone(),
        blocks: None,
        thread_ts: None,
    }
}

// ---------------------------------------------------------------------------
// Notifier trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl<H: SlackHttpClient + 'static> Notifier for SlackNotifier<H> {
    fn name(&self) -> &str {
        "slack"
    }

    fn is_enabled(&self) -> bool {
        self.config.webhook_url.is_some() || self.has_bot_channel()
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

    async fn notify_merged(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let mention = self.get_user_mention_for_issue(issue);
        self.send(build_merged_message(issue, pr_url, mention))
            .await
    }

    async fn notify_report(&self, report: &Report) -> Result<()> {
        self.send(build_report_message(report)).await
    }

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let mention = self.get_user_mention_for_issue(issue);
        let message = build_ask_question_message(issue, request, mention);

        // Always use chat.postMessage for Q&A so we get the ts for threading.
        let ts = self.send_to_channel(message).await?;

        Ok(Some(AskDelivery {
            channel: "slack".to_string(),
            target: self.get_target_slack_id_for_issue(issue),
            message_id: ts,
        }))
    }

    async fn poll_question_replies(
        &self,
        request: &AskRequest,
        since: DateTime<Utc>,
    ) -> Result<Vec<AskReply>> {
        let token_str = match self.config.bot_token.as_deref() {
            Some(t) if !t.is_empty() => t,
            _ => return Ok(Vec::new()),
        };
        let channel_id = match self.config.channel_id.as_deref() {
            Some(c) if !c.is_empty() => c,
            _ => return Ok(Vec::new()),
        };

        let expected_user = self.expected_reply_user_id(request);
        let correlation_token = format!("[CLAUDEAR-Q:{}]", request.correlation_id);

        // Step 1: Fetch recent channel history to find our question messages.
        let since_ts = format!("{}.000000", since.timestamp());
        let history_url = format!(
            "{}conversations.history?channel={}&oldest={}&limit=50",
            SLACK_API_BASE,
            urlencoding::encode(channel_id),
            urlencoding::encode(&since_ts)
        );
        let history_resp = self.http.get_json(&history_url, Some(token_str)).await?;
        let history: SlackApiResponse =
            serde_json::from_str(&history_resp.body).unwrap_or(SlackApiResponse {
                ok: false,
                error: None,
                ts: None,
                messages: None,
            });

        if !history.ok {
            tracing::warn!("Slack conversations.history failed: {:?}", history.error);
            return Ok(Vec::new());
        }

        let messages = history.messages.unwrap_or_default();

        // Find question messages with our correlation token.
        let question_messages: Vec<&SlackApiMessage> = messages
            .iter()
            .filter(|m| m.text.contains(&correlation_token))
            .collect();

        let mut replies: Vec<AskReply> = Vec::new();

        // Step 2: For each question message, fetch its thread replies.
        for qm in &question_messages {
            let replies_url = format!(
                "{}conversations.replies?channel={}&ts={}&oldest={}",
                SLACK_API_BASE,
                urlencoding::encode(channel_id),
                urlencoding::encode(&qm.ts),
                urlencoding::encode(&since_ts)
            );
            let replies_resp = self.http.get_json(&replies_url, Some(token_str)).await?;
            let thread: SlackApiResponse =
                serde_json::from_str(&replies_resp.body).unwrap_or(SlackApiResponse {
                    ok: false,
                    error: None,
                    ts: None,
                    messages: None,
                });

            if !thread.ok {
                continue;
            }

            let thread_messages = thread.messages.unwrap_or_default();

            for tm in thread_messages {
                // Skip the parent message itself.
                if tm.ts == qm.ts {
                    continue;
                }
                // Skip bot messages.
                if tm.bot_id.is_some() {
                    continue;
                }
                let user_id = match tm.user {
                    Some(ref u) => u.clone(),
                    None => continue,
                };

                // Filter by expected user if configured.
                if let Some(ref expected) = expected_user {
                    if &user_id != expected {
                        continue;
                    }
                }

                let parsed_time = match slack_ts_to_datetime(&tm.ts) {
                    Some(dt) => dt,
                    None => continue,
                };

                if parsed_time < since {
                    continue;
                }

                let answer = match Self::extract_reply_text(&tm.text) {
                    Some(a) => a,
                    None => continue,
                };

                replies.push(AskReply {
                    correlation_id: request.correlation_id.clone(),
                    channel: "slack".to_string(),
                    responder: Some(user_id),
                    answer,
                    replied_at: parsed_time,
                });
            }
        }

        // Also check for top-level messages containing the token (non-threaded replies).
        for msg in &messages {
            // Skip our own question messages.
            if msg.text.contains(&correlation_token) && msg.bot_id.is_some() {
                continue;
            }
            if msg.bot_id.is_some() {
                continue;
            }
            let user_id = match msg.user {
                Some(ref u) => u.clone(),
                None => continue,
            };
            if let Some(ref expected) = expected_user {
                if &user_id != expected {
                    continue;
                }
            }
            let parsed_time = match slack_ts_to_datetime(&msg.ts) {
                Some(dt) => dt,
                None => continue,
            };
            if parsed_time < since {
                continue;
            }

            // Must contain our token (non-threaded reply convention).
            if let Some(answer) =
                Self::extract_reply_text_with_token(&msg.text, &request.correlation_id)
            {
                replies.push(AskReply {
                    correlation_id: request.correlation_id.clone(),
                    channel: "slack".to_string(),
                    responder: Some(user_id),
                    answer,
                    replied_at: parsed_time,
                });
            }
        }

        replies.sort_by_key(|r| r.replied_at);
        Ok(replies)
    }

    fn supports_replies(&self) -> bool {
        self.has_bot_channel()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn empty_registry() -> UserRegistry {
        UserRegistry::new(std::collections::HashMap::new())
    }

    // -----------------------------------------------------------------------
    // Mock HTTP client
    // -----------------------------------------------------------------------

    /// Mock Slack HTTP client that records calls and returns configurable responses.
    struct MockSlackHttpClient {
        response_status: u16,
        response_body: String,
        call_count: AtomicUsize,
        last_post_calls: Mutex<Vec<(String, serde_json::Value, Option<String>)>>,
        last_get_calls: Mutex<Vec<(String, Option<String>)>>,
        /// Optional per-URL response overrides (url prefix -> response body).
        get_responses: Mutex<Vec<(String, String)>>,
    }

    impl MockSlackHttpClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response_status: status,
                response_body: body.to_string(),
                call_count: AtomicUsize::new(0),
                last_post_calls: Mutex::new(Vec::new()),
                last_get_calls: Mutex::new(Vec::new()),
                get_responses: Mutex::new(Vec::new()),
            }
        }

        fn success() -> Self {
            // Slack webhook returns "ok" as plain text on success.
            Self::new(200, "ok")
        }

        fn success_api() -> Self {
            Self::new(200, r#"{"ok":true,"ts":"1709123456.789012"}"#)
        }

        fn error(status: u16, body: &str) -> Self {
            Self::new(status, body)
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn get_last_post_call(&self) -> Option<(String, serde_json::Value, Option<String>)> {
            self.last_post_calls.lock().unwrap().last().cloned()
        }

        #[allow(dead_code)]
        fn get_last_get_call(&self) -> Option<(String, Option<String>)> {
            self.last_get_calls.lock().unwrap().last().cloned()
        }

        fn add_get_response(&self, url_prefix: &str, body: &str) {
            self.get_responses
                .lock()
                .unwrap()
                .push((url_prefix.to_string(), body.to_string()));
        }
    }

    #[async_trait]
    impl SlackHttpClient for MockSlackHttpClient {
        async fn post_json(
            &self,
            url: &str,
            body: &serde_json::Value,
            auth_token: Option<&str>,
        ) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.last_post_calls.lock().unwrap().push((
                url.to_string(),
                body.clone(),
                auth_token.map(|s| s.to_string()),
            ));

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }

        async fn get_json(&self, url: &str, auth_token: Option<&str>) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.last_get_calls
                .lock()
                .unwrap()
                .push((url.to_string(), auth_token.map(|s| s.to_string())));

            // Check for URL-specific responses.
            let responses = self.get_responses.lock().unwrap();
            for (prefix, body) in responses.iter() {
                if url.starts_with(prefix) || url.contains(prefix) {
                    return Ok(HttpResponse {
                        status: self.response_status,
                        body: body.clone(),
                    });
                }
            }

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }
    }

    // -----------------------------------------------------------------------
    // Config helpers
    // -----------------------------------------------------------------------

    fn webhook_config() -> SlackConfig {
        SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            ..Default::default()
        }
    }

    fn webhook_config_with_user() -> SlackConfig {
        SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            user_id: Some("U987654321".to_string()),
            ..Default::default()
        }
    }

    fn bot_config() -> SlackConfig {
        SlackConfig {
            bot_token: Some("xoxb-test-token".to_string()),
            channel_id: Some("C12345678".to_string()),
            ..Default::default()
        }
    }

    fn bot_config_with_user() -> SlackConfig {
        SlackConfig {
            bot_token: Some("xoxb-test-token".to_string()),
            channel_id: Some("C12345678".to_string()),
            user_id: Some("U987654321".to_string()),
            ..Default::default()
        }
    }

    fn test_issue() -> Issue {
        Issue::new(
            "123",
            "PROJ-123",
            "Test Issue Title",
            "https://example.com/issue/123",
            "linear",
        )
    }

    fn make_ask_request(
        correlation_id: &str,
        question: &str,
        context: Option<&str>,
        options: Vec<&str>,
        why: Option<&str>,
        target_slack_id: Option<&str>,
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
            target_discord_id: None,
            target_email: None,
            target_slack_id: target_slack_id.map(|s| s.to_string()),
        }
    }

    // -----------------------------------------------------------------------
    // Basic tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_notifier_name() {
        let config = SlackConfig::default();
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert_eq!(notifier.name(), "slack");
    }

    #[test]
    fn test_is_enabled_with_webhook() {
        let notifier =
            SlackNotifier::with_http_client(webhook_config(), MockSlackHttpClient::success());
        assert!(notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_with_bot_channel() {
        let notifier =
            SlackNotifier::with_http_client(bot_config(), MockSlackHttpClient::success_api());
        assert!(notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_when_nothing_configured() {
        let config = SlackConfig::default();
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_with_only_bot_token() {
        let config = SlackConfig {
            bot_token: Some("xoxb-token".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_with_only_channel_id() {
        let config = SlackConfig {
            channel_id: Some("C12345678".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_with_empty_bot_token() {
        let config = SlackConfig {
            bot_token: Some("".to_string()),
            channel_id: Some("C12345678".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_false_with_empty_channel_id() {
        let config = SlackConfig {
            bot_token: Some("xoxb-token".to_string()),
            channel_id: Some("".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.is_enabled());
    }

    // -----------------------------------------------------------------------
    // Webhook send tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_send_webhook_success() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let result = notifier.notify_start(&test_issue()).await;
        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_send_webhook_sends_to_correct_url() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier.notify_start(&test_issue()).await.unwrap();
        let (url, _, auth) = notifier.http.get_last_post_call().unwrap();
        assert_eq!(url, "https://hooks.slack.com/services/T00/B00/xxx");
        assert!(
            auth.is_none(),
            "Webhook calls should not include auth token"
        );
    }

    #[tokio::test]
    async fn test_send_webhook_error_response() {
        let mock = MockSlackHttpClient::error(400, "invalid_payload");
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let result = notifier.notify_start(&test_issue()).await;
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("Webhook error"));
    }

    #[tokio::test]
    async fn test_send_webhook_server_error() {
        let mock = MockSlackHttpClient::error(500, "Internal Server Error");
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let result = notifier.notify_status("Test").await;
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // Bot API send tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_send_bot_api_success() {
        let mock = MockSlackHttpClient::success_api();
        let notifier = SlackNotifier::with_http_client(bot_config(), mock);
        let result = notifier.notify_start(&test_issue()).await;
        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_send_bot_api_includes_auth_token() {
        let mock = MockSlackHttpClient::success_api();
        let notifier = SlackNotifier::with_http_client(bot_config(), mock);
        notifier.notify_start(&test_issue()).await.unwrap();
        let (url, body, auth) = notifier.http.get_last_post_call().unwrap();
        assert!(url.contains("chat.postMessage"));
        assert_eq!(auth.as_deref(), Some("xoxb-test-token"));
        assert_eq!(body["channel"], "C12345678");
    }

    #[tokio::test]
    async fn test_send_bot_api_error_response() {
        let mock = MockSlackHttpClient::new(200, r#"{"ok":false,"error":"channel_not_found"}"#);
        let notifier = SlackNotifier::with_http_client(bot_config(), mock);
        let result = notifier.notify_start(&test_issue()).await;
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("channel_not_found"));
    }

    // -----------------------------------------------------------------------
    // Notification message content tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_notify_start_sends_blocks() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier.notify_start(&test_issue()).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        assert!(body["blocks"].is_array());
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("PROJ-123"));
        assert!(text.contains("Processing"));
    }

    #[tokio::test]
    async fn test_notify_start_with_user_mention() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config_with_user(), mock);
        notifier.notify_start(&test_issue()).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("<@U987654321>"));
    }

    #[tokio::test]
    async fn test_notify_success_sends_correct_content() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier
            .notify_success(&test_issue(), "https://github.com/org/repo/pull/42")
            .await
            .unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("PR Created"));
        assert!(text.contains("PROJ-123"));
    }

    #[tokio::test]
    async fn test_notify_completed_sends_correct_content() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier.notify_completed(&test_issue()).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("Completed"));
    }

    #[tokio::test]
    async fn test_notify_failed_sends_correct_content() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier
            .notify_failed(&test_issue(), "Build failed with exit code 1")
            .await
            .unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("Failed"));
        assert!(text.contains("PROJ-123"));
    }

    #[tokio::test]
    async fn test_notify_failed_truncates_long_error() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let long_error = "x".repeat(2000);
        notifier
            .notify_failed(&test_issue(), &long_error)
            .await
            .unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        // The blocks should contain a truncated error
        let blocks = body["blocks"].as_array().unwrap();
        let error_block = blocks
            .iter()
            .find(|b| {
                b.get("text")
                    .and_then(|t| t.get("text"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.contains("Error"))
                    .unwrap_or(false)
            })
            .unwrap();
        let error_text = error_block["text"]["text"].as_str().unwrap();
        assert!(error_text.contains("..."));
        // Error display should be truncated to ~1000 chars (within the code block markers)
        assert!(error_text.len() <= 1100);
    }

    #[tokio::test]
    async fn test_notify_status_sends_content() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        notifier.notify_status("System is healthy").await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert_eq!(text, "System is healthy");
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
        // No call should have been made
        assert_eq!(notifier.http.get_call_count(), 0);
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_sends_content() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com/1", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com/2", "sentry"),
        ];
        notifier.notify_urgent_issues(&issues).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("2 Urgent Issues"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_single_item_grammar() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "linear",
        )];
        notifier.notify_urgent_issues(&issues).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("1 Urgent Issue Detected"));
        assert!(!text.contains("Issues"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_with_user_mention() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config_with_user(), mock);
        let issues = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "linear",
        )];
        notifier.notify_urgent_issues(&issues).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("<@U987654321>"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_limits_to_ten() {
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(webhook_config(), mock);
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
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let blocks = body["blocks"].as_array().unwrap();
        // Count section blocks that are issue items (after header + description).
        let issue_blocks: Vec<_> = blocks
            .iter()
            .filter(|b| {
                b.get("type").and_then(|t| t.as_str()) == Some("section")
                    && b.get("text")
                        .and_then(|t| t.get("text"))
                        .and_then(|t| t.as_str())
                        .map(|s| s.contains("PROJ-"))
                        .unwrap_or(false)
            })
            .collect();
        assert!(issue_blocks.len() <= 10);
    }

    // -----------------------------------------------------------------------
    // Disabled notifier tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let config = SlackConfig::default();
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        let result = notifier.notify_status("Test status").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let config = SlackConfig::default();
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        let result = notifier.notify_start(&test_issue()).await;
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // User mention tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_user_mention() {
        let notifier = SlackNotifier::with_http_client(
            webhook_config_with_user(),
            MockSlackHttpClient::success(),
        );
        assert_eq!(
            notifier.get_user_mention(),
            Some("<@U987654321>".to_string())
        );
    }

    #[test]
    fn test_user_mention_none() {
        let notifier =
            SlackNotifier::with_http_client(webhook_config(), MockSlackHttpClient::success());
        assert_eq!(notifier.get_user_mention(), None);
    }

    #[tokio::test]
    async fn test_notify_start_with_resolved_user_mention() {
        let mock = MockSlackHttpClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                slack_id: Some("U111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = test_issue();
        issue.set_metadata("resolved_user", "jake");
        notifier.notify_start(&issue).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("<@U111222333>"));
    }

    #[tokio::test]
    async fn test_resolved_user_overrides_global_user_id() {
        let mock = MockSlackHttpClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                slack_id: Some("U111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            user_id: Some("U999999999".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = test_issue();
        issue.set_metadata("resolved_user", "jake");
        notifier.notify_start(&issue).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("<@U111222333>"));
        assert!(!text.contains("<@U999999999>"));
    }

    #[tokio::test]
    async fn test_fallback_to_global_when_no_resolved_user() {
        let mock = MockSlackHttpClient::success();
        let registry = empty_registry();
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            user_id: Some("U999999999".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(config, mock, registry);
        notifier.notify_start(&test_issue()).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("<@U999999999>"));
    }

    #[test]
    fn test_get_user_mention_for_issue_no_resolved_user_no_global() {
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        let issue = test_issue();
        assert_eq!(notifier.get_user_mention_for_issue(&issue), None);
    }

    #[test]
    fn test_get_user_mention_for_issue_resolved_user_no_slack_id() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                slack_id: None,
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            user_id: Some("Ufallback".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(
            config,
            MockSlackHttpClient::success(),
            registry,
        );
        let mut issue = test_issue();
        issue.set_metadata("resolved_user", "jake");
        assert_eq!(
            notifier.get_user_mention_for_issue(&issue),
            Some("<@Ufallback>".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // Q&A tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_ask_question_sends_via_chat_post_message() {
        let mock = MockSlackHttpClient::success_api();
        let notifier = SlackNotifier::with_http_client(bot_config_with_user(), mock);
        let issue = test_issue();
        let request = make_ask_request("tok-1", "Choose target branch?", None, vec![], None, None);
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.channel, "slack");
        assert_eq!(delivery.message_id.as_deref(), Some("1709123456.789012"));
    }

    #[tokio::test]
    async fn test_ask_question_includes_correlation_token() {
        let mock = MockSlackHttpClient::success_api();
        let notifier = SlackNotifier::with_http_client(bot_config(), mock);
        let issue = test_issue();
        let request = make_ask_request(
            "tok-abc",
            "Pick a branch",
            None,
            vec!["main", "develop"],
            None,
            None,
        );
        notifier.ask_question(&issue, &request).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("[CLAUDEAR-Q:tok-abc]"));
        assert!(text.contains("Pick a branch"));
        assert!(text.contains("main | develop"));
    }

    #[tokio::test]
    async fn test_ask_question_includes_options_and_context() {
        let mock = MockSlackHttpClient::success_api();
        let notifier = SlackNotifier::with_http_client(bot_config(), mock);
        let issue = test_issue();
        let request = make_ask_request(
            "tok-opts",
            "Pick a branch",
            Some("We need a target for the PR"),
            vec!["main", "develop"],
            Some("Multiple branches available"),
            None,
        );
        notifier.ask_question(&issue, &request).await.unwrap();
        let (_, body, _) = notifier.http.get_last_post_call().unwrap();
        let text = body["text"].as_str().unwrap();
        assert!(text.contains("[CLAUDEAR-Q:tok-opts]"));
        assert!(text.contains("Pick a branch"));
        assert!(text.contains("Why: Multiple branches available"));
        assert!(text.contains("Context: We need a target for the PR"));
        assert!(text.contains("main | develop"));
    }

    #[tokio::test]
    async fn test_ask_question_uses_resolved_user_target() {
        let mock = MockSlackHttpClient::success_api();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                slack_id: Some("U111222333".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = SlackConfig {
            bot_token: Some("xoxb-test-token".to_string()),
            channel_id: Some("C12345678".to_string()),
            user_id: Some("U999999999".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(config, mock, registry);
        let mut issue = test_issue();
        issue.set_metadata("resolved_user", "jake");
        let request = make_ask_request("tok-1", "Question?", None, vec![], None, None);
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("U111222333"));
    }

    #[tokio::test]
    async fn test_ask_question_falls_back_to_global_target() {
        let mock = MockSlackHttpClient::success_api();
        let config = SlackConfig {
            bot_token: Some("xoxb-test-token".to_string()),
            channel_id: Some("C12345678".to_string()),
            user_id: Some("U999999999".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client_and_registry(config, mock, empty_registry());
        let issue = test_issue();
        let request = make_ask_request("tok-2", "Question?", None, vec![], None, None);
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("U999999999"));
    }

    #[tokio::test]
    async fn test_poll_question_replies_basic() {
        let mock = MockSlackHttpClient::success_api();
        let since = Utc::now() - chrono::Duration::minutes(5);

        // History response: one question message from our bot.
        let history_body = serde_json::json!({
            "ok": true,
            "messages": [
                {
                    "ts": "1709123456.000000",
                    "text": "[CLAUDEAR-Q:corr-1] Human input needed for PROJ-123:\nPick a branch",
                    "bot_id": "B12345",
                    "user": null
                }
            ]
        });
        // Replies response: one human reply in the thread.
        let reply_ts_f = (Utc::now().timestamp() as f64) + 10.0;
        let reply_ts = format!("{:.6}", reply_ts_f);
        let replies_body = serde_json::json!({
            "ok": true,
            "messages": [
                {
                    "ts": "1709123456.000000",
                    "text": "[CLAUDEAR-Q:corr-1] Human input needed",
                    "bot_id": "B12345"
                },
                {
                    "ts": reply_ts,
                    "text": "Use main branch",
                    "user": "U987654321"
                }
            ]
        });

        mock.add_get_response("conversations.history", &history_body.to_string());
        mock.add_get_response("conversations.replies", &replies_body.to_string());

        let notifier = SlackNotifier::with_http_client(bot_config_with_user(), mock);
        let request = make_ask_request("corr-1", "Pick a branch", None, vec![], None, None);
        let replies = notifier
            .poll_question_replies(&request, since)
            .await
            .unwrap();

        assert_eq!(replies.len(), 1);
        assert_eq!(replies[0].answer, "Use main branch");
        assert_eq!(replies[0].channel, "slack");
        assert_eq!(replies[0].responder.as_deref(), Some("U987654321"));
        assert_eq!(replies[0].correlation_id, "corr-1");
    }

    #[tokio::test]
    async fn test_poll_question_replies_filters_by_user() {
        let mock = MockSlackHttpClient::success_api();
        let since = Utc::now() - chrono::Duration::minutes(5);

        let history_body = serde_json::json!({
            "ok": true,
            "messages": [
                {
                    "ts": "1709123456.000000",
                    "text": "[CLAUDEAR-Q:corr-2] Question",
                    "bot_id": "B12345"
                }
            ]
        });
        let reply_ts_f = (Utc::now().timestamp() as f64) + 10.0;
        let reply_ts = format!("{:.6}", reply_ts_f);
        let replies_body = serde_json::json!({
            "ok": true,
            "messages": [
                {
                    "ts": "1709123456.000000",
                    "text": "[CLAUDEAR-Q:corr-2] Question",
                    "bot_id": "B12345"
                },
                {
                    "ts": reply_ts,
                    "text": "Wrong user reply",
                    "user": "U_OTHER"
                }
            ]
        });

        mock.add_get_response("conversations.history", &history_body.to_string());
        mock.add_get_response("conversations.replies", &replies_body.to_string());

        let notifier = SlackNotifier::with_http_client(bot_config_with_user(), mock);
        let request = make_ask_request(
            "corr-2",
            "Question?",
            None,
            vec![],
            None,
            Some("U987654321"),
        );
        let replies = notifier
            .poll_question_replies(&request, since)
            .await
            .unwrap();

        // Should be empty because the reply is from a different user.
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_returns_empty_without_bot_token() {
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            ..Default::default()
        };
        let mock = MockSlackHttpClient::success();
        let notifier = SlackNotifier::with_http_client(config, mock);
        let request = make_ask_request("corr-3", "Q?", None, vec![], None, None);
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    // -----------------------------------------------------------------------
    // supports_replies
    // -----------------------------------------------------------------------

    #[test]
    fn test_supports_replies_true_when_both_set() {
        let notifier =
            SlackNotifier::with_http_client(bot_config(), MockSlackHttpClient::success());
        assert!(notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_bot_token() {
        let config = SlackConfig {
            webhook_url: Some("https://hooks.slack.com/services/T00/B00/xxx".to_string()),
            channel_id: Some("C12345678".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_channel_id() {
        let config = SlackConfig {
            bot_token: Some("xoxb-token".to_string()),
            ..Default::default()
        };
        let notifier = SlackNotifier::with_http_client(config, MockSlackHttpClient::success());
        assert!(!notifier.supports_replies());
    }

    // -----------------------------------------------------------------------
    // Expected reply user
    // -----------------------------------------------------------------------

    #[test]
    fn test_expected_reply_user_id_prefers_request_target() {
        let notifier =
            SlackNotifier::with_http_client(bot_config_with_user(), MockSlackHttpClient::success());
        let request = make_ask_request("tok-1", "Q?", None, vec![], None, Some("U_REQUEST_TARGET"));
        assert_eq!(
            notifier.expected_reply_user_id(&request),
            Some("U_REQUEST_TARGET".to_string())
        );
    }

    #[test]
    fn test_expected_reply_user_id_falls_back_to_config() {
        let notifier =
            SlackNotifier::with_http_client(bot_config_with_user(), MockSlackHttpClient::success());
        let request = make_ask_request("tok-2", "Q?", None, vec![], None, None);
        assert_eq!(
            notifier.expected_reply_user_id(&request),
            Some("U987654321".to_string())
        );
    }

    #[test]
    fn test_expected_reply_user_id_none_when_both_absent() {
        let notifier =
            SlackNotifier::with_http_client(bot_config(), MockSlackHttpClient::success());
        let request = make_ask_request("tok-3", "Q?", None, vec![], None, None);
        assert_eq!(notifier.expected_reply_user_id(&request), None);
    }

    // -----------------------------------------------------------------------
    // Extract reply text helpers
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_reply_text() {
        let parsed =
            SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text("Use main branch").unwrap();
        assert_eq!(parsed, "Use main branch");
    }

    #[test]
    fn test_extract_reply_text_empty_string() {
        let result = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text("");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_whitespace_only() {
        let result = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text("   \n\t  ");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_trims_whitespace() {
        let result =
            SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text("  yes  ").unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_extract_reply_text_with_token() {
        let parsed = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text_with_token(
            "[CLAUDEAR-Q:abc123] Use main branch",
            "abc123",
        )
        .unwrap();
        assert_eq!(parsed, "Use main branch");
    }

    #[test]
    fn test_extract_reply_text_with_token_wrong_id_returns_none() {
        let result = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text_with_token(
            "[CLAUDEAR-Q:abc123] Use main branch",
            "wrong-id",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_only_token_returns_none() {
        let result = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text_with_token(
            "[CLAUDEAR-Q:abc123]",
            "abc123",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_reply_text_with_token_no_token_at_all() {
        let result = SlackNotifier::<ReqwestSlackHttpClient>::extract_reply_text_with_token(
            "just a regular message",
            "abc123",
        );
        assert!(result.is_none());
    }

    // -----------------------------------------------------------------------
    // Truncation tests
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // slack_ts_to_datetime tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_slack_ts_to_datetime_valid() {
        let dt = slack_ts_to_datetime("1709123456.789012").unwrap();
        assert_eq!(dt.timestamp(), 1709123456);
    }

    #[test]
    fn test_slack_ts_to_datetime_invalid() {
        assert!(slack_ts_to_datetime("not_a_timestamp").is_none());
    }

    #[test]
    fn test_slack_ts_to_datetime_empty() {
        assert!(slack_ts_to_datetime("").is_none());
    }

    // -----------------------------------------------------------------------
    // Block Kit serialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_slack_text_mrkdwn() {
        let t = SlackText::mrkdwn("*bold*");
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("mrkdwn"));
        assert!(json.contains("*bold*"));
    }

    #[test]
    fn test_slack_text_plain_text() {
        let t = SlackText::plain_text("Hello");
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("plain_text"));
        assert!(json.contains("Hello"));
    }

    #[test]
    fn test_slack_block_header_serialization() {
        let block = SlackBlock::Header {
            text: SlackText::plain_text("Title"),
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"header\""));
        assert!(json.contains("Title"));
    }

    #[test]
    fn test_slack_block_divider_serialization() {
        let block = SlackBlock::Divider;
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"divider\""));
    }

    #[test]
    fn test_slack_message_serialization() {
        let msg = SlackMessage {
            channel: Some("C123".to_string()),
            text: "fallback".to_string(),
            blocks: None,
            thread_ts: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("C123"));
        assert!(json.contains("fallback"));
        // Optional None fields should be skipped.
        assert!(!json.contains("blocks"));
        assert!(!json.contains("thread_ts"));
    }

    #[test]
    fn test_slack_api_response_deserialization() {
        let json = r#"{"ok":true,"ts":"1709123456.789012"}"#;
        let resp: SlackApiResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.ts.as_deref(), Some("1709123456.789012"));
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_slack_api_response_error_deserialization() {
        let json = r#"{"ok":false,"error":"channel_not_found"}"#;
        let resp: SlackApiResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error.as_deref(), Some("channel_not_found"));
    }

    #[test]
    fn test_slack_api_message_deserialization() {
        let json = r#"{"ts":"1709123456.000000","text":"hello","user":"U123","bot_id":null,"thread_ts":"1709123400.000000"}"#;
        let msg: SlackApiMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, "1709123456.000000");
        assert_eq!(msg.text, "hello");
        assert_eq!(msg.user.as_deref(), Some("U123"));
        assert!(msg.bot_id.is_none());
        assert_eq!(msg.thread_ts.as_deref(), Some("1709123400.000000"));
    }

    // -----------------------------------------------------------------------
    // ReqwestSlackHttpClient default
    // -----------------------------------------------------------------------

    #[test]
    fn test_reqwest_slack_http_client_default() {
        let client = ReqwestSlackHttpClient::default();
        assert!(std::mem::size_of_val(&client) > 0);
    }
}

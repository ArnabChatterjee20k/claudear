//! Slack Events API webhook handler.

use super::WebhookHandler;
use crate::config::SlackSourceConfig;
use crate::error::Result;
use crate::secret::OptionalSecretExt;
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::HashMap;
use subtle::ConstantTimeEq;

/// Slack webhook handler for message events.
pub struct SlackWebhookHandler {
    config: SlackSourceConfig,
}

impl SlackWebhookHandler {
    /// Create a new Slack webhook handler.
    pub fn new(config: SlackSourceConfig) -> Self {
        Self { config }
    }

    fn listen_channel_id(&self) -> Option<&str> {
        self.config
            .listen_channel_id
            .as_deref()
            .or(self.config.channel_id.as_deref())
    }

    fn message_url(&self, channel_id: &str, ts: &str) -> String {
        let ts_nodot = ts.replace('.', "");
        match &self.config.workspace {
            Some(workspace) => format!(
                "https://{}.slack.com/archives/{}/p{}",
                workspace, channel_id, ts_nodot
            ),
            None => format!("https://slack.com/archives/{}/p{}", channel_id, ts_nodot),
        }
    }

    fn extract_title(content: &str) -> String {
        let first_line = content.lines().next().unwrap_or(content);
        if first_line.len() > 100 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(97)])
        } else {
            first_line.to_string()
        }
    }
}

#[async_trait]
impl WebhookHandler for SlackWebhookHandler {
    fn source_name(&self) -> &str {
        "slack"
    }

    fn verify_signature(&self, payload: &[u8], headers: &HashMap<String, String>) -> bool {
        let secret = match self.config.signing_secret.expose_as_deref() {
            Some(s) if !s.is_empty() => s,
            _ => {
                tracing::error!(
                    source = "slack",
                    "No Slack signing_secret configured - rejecting request"
                );
                return false;
            }
        };

        let timestamp = match headers.get("x-slack-request-timestamp") {
            Some(v) => v,
            None => return false,
        };
        let signature = match headers.get("x-slack-signature") {
            Some(v) => v,
            None => return false,
        };

        let ts = match timestamp.parse::<i64>() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let now = Utc::now().timestamp();
        if (now - ts).abs() > 60 * 5 {
            tracing::warn!(
                source = "slack",
                "Slack webhook timestamp outside replay window"
            );
            return false;
        }

        let mut mac = match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
            Ok(m) => m,
            Err(_) => return false,
        };
        let base = format!("v0:{}:", timestamp);
        mac.update(base.as_bytes());
        mac.update(payload);
        let expected = format!("v0={}", hex::encode(mac.finalize().into_bytes()));

        signature.as_bytes().ct_eq(expected.as_bytes()).into()
    }

    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>> {
        let payload_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if payload_type == "url_verification" {
            return Ok(None);
        }
        if payload_type != "event_callback" {
            return Ok(None);
        }

        let event = match payload.get("event") {
            Some(v) => v,
            None => return Ok(None),
        };

        if event.get("type").and_then(|v| v.as_str()) != Some("message") {
            return Ok(None);
        }

        if event.get("bot_id").is_some() {
            return Ok(None);
        }
        if event.get("subtype").is_some() {
            return Ok(None);
        }

        let channel_id = match event.get("channel").and_then(|v| v.as_str()) {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(None),
        };
        if let Some(expected_channel) = self.listen_channel_id() {
            if channel_id != expected_channel {
                return Ok(None);
            }
        }

        let text = match event.get("text").and_then(|v| v.as_str()) {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => return Ok(None),
        };
        let ts = match event.get("ts").and_then(|v| v.as_str()) {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(None),
        };

        let short_id = format!("SLACK-{}", ts.chars().take(8).collect::<String>());
        let title = Self::extract_title(&text);
        let url = self.message_url(channel_id, ts);
        let mut issue = Issue::new(ts, short_id, title, url, "slack");
        issue.description = Some(text);
        issue.set_metadata("channel_id", channel_id);
        issue.set_metadata("message_ts", ts);

        if let Some(user_id) = event.get("user").and_then(|v| v.as_str()) {
            issue.set_metadata("author_id", user_id);
        }

        Ok(Some(issue))
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        MatchResult::matched("slack_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("Slack Message Issue: {}\n", issue.title);
        if let Some(desc) = &issue.description {
            context.push_str("\nMessage:\n");
            context.push_str(desc);
            context.push('\n');
        }
        if let Some(author_id) = issue.get_metadata::<String>("author_id") {
            context.push_str(&format!("\nAuthor ID: {}\n", author_id));
        }
        if let Some(channel_id) = issue.get_metadata::<String>("channel_id") {
            context.push_str(&format!("Channel: {}\n", channel_id));
        }
        Ok(context)
    }
}

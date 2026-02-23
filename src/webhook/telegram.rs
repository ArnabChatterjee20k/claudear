//! Telegram webhook handler.

use super::WebhookHandler;
use crate::ask_reply_inbox;
use crate::config::TelegramConfig;
use crate::error::Result;
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use std::collections::HashMap;
use subtle::ConstantTimeEq;

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    #[serde(default)]
    message: Option<TelegramMessage>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    chat: TelegramChat,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    date: Option<i64>,
    #[serde(default)]
    reply_to_message: Option<TelegramReplyMessage>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramReplyMessage {
    message_id: i64,
    #[serde(default)]
    from: Option<TelegramUser>,
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct TelegramUser {
    id: i64,
    is_bot: bool,
    #[serde(default)]
    username: Option<String>,
}

/// Webhook handler for Telegram Bot API updates.
pub struct TelegramWebhookHandler {
    config: TelegramConfig,
}

impl TelegramWebhookHandler {
    /// Create a new Telegram webhook handler.
    pub fn new(config: TelegramConfig) -> Self {
        Self { config }
    }

    fn listen_chat_id(&self) -> Option<i64> {
        self.config
            .listen_chat_id
            .as_deref()
            .or(self.config.chat_id.as_deref())
            .and_then(|v| v.parse::<i64>().ok())
    }

    fn message_url(chat_id: i64, message_id: i64) -> String {
        if chat_id < -1_000_000_000_000 {
            let channel_id = (-chat_id) - 1_000_000_000_000;
            format!("https://t.me/c/{}/{}", channel_id, message_id)
        } else {
            String::new()
        }
    }

    fn extract_title(text: &str) -> String {
        let first_line = text.lines().next().unwrap_or(text);
        if first_line.len() > 100 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(97)])
        } else {
            first_line.to_string()
        }
    }

    fn record_ask_reply_candidate(msg: &TelegramMessage) {
        let text = match msg.text.as_ref() {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => return,
        };
        let from = match msg.from.as_ref() {
            Some(v) if !v.is_bot => v,
            _ => return,
        };

        let replied_at = msg
            .date
            .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
            .unwrap_or_else(Utc::now);

        ask_reply_inbox::record_telegram_message(ask_reply_inbox::TelegramInboundMessage {
            message_id: msg.message_id,
            chat_id: msg.chat.id,
            responder_id: Some(from.id.to_string()),
            responder_username: from.username.clone(),
            text,
            replied_at,
            reply_to_message_id: msg.reply_to_message.as_ref().map(|m| m.message_id),
            reply_to_text: msg.reply_to_message.as_ref().and_then(|m| m.text.clone()),
            reply_to_is_bot: msg
                .reply_to_message
                .as_ref()
                .and_then(|m| m.from.as_ref().map(|u| u.is_bot)),
        });
    }
}

#[async_trait]
impl WebhookHandler for TelegramWebhookHandler {
    fn source_name(&self) -> &str {
        "telegram"
    }

    fn verify_signature(&self, _payload: &[u8], headers: &HashMap<String, String>) -> bool {
        // Telegram webhook signatures are optional. If a secret token is present in env,
        // require the matching `X-Telegram-Bot-Api-Secret-Token` header.
        let secret = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok();
        let secret = secret.as_deref().map(str::trim).filter(|s| !s.is_empty());
        let Some(secret) = secret else {
            return true;
        };

        let provided = match headers.get("x-telegram-bot-api-secret-token") {
            Some(v) => v.as_str(),
            None => return false,
        };
        provided.as_bytes().ct_eq(secret.as_bytes()).into()
    }

    async fn parse_payload(&self, payload: &serde_json::Value) -> Result<Option<Issue>> {
        let update: TelegramUpdate = serde_json::from_value(payload.clone())?;
        let msg = match update.message {
            Some(m) => m,
            None => return Ok(None),
        };

        if let Some(expected_chat_id) = self.listen_chat_id() {
            if msg.chat.id != expected_chat_id {
                return Ok(None);
            }
        }

        let from = match msg.from.as_ref() {
            Some(v) if !v.is_bot => v,
            _ => return Ok(None),
        };

        let text = match msg.text.as_ref() {
            Some(v) if !v.trim().is_empty() => v.trim().to_string(),
            _ => return Ok(None),
        };

        Self::record_ask_reply_candidate(&msg);

        let short_id = format!("TG-{}", msg.message_id);
        let title = Self::extract_title(&text);
        let url = Self::message_url(msg.chat.id, msg.message_id);

        let mut issue = Issue::new(msg.message_id.to_string(), short_id, title, url, "telegram");
        issue.description = Some(text);
        issue.set_metadata("chat_id", msg.chat.id);
        issue.set_metadata("message_id", msg.message_id);
        issue.set_metadata("author_id", from.id);
        if let Some(username) = &from.username {
            issue.set_metadata("author_username", username);
        }

        Ok(Some(issue))
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        MatchResult::matched("telegram_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("Telegram Message Issue: {}\n", issue.title);
        if let Some(desc) = &issue.description {
            context.push_str("\nMessage:\n");
            context.push_str(desc);
            context.push('\n');
        }
        if let Some(username) = issue.get_metadata::<String>("author_username") {
            context.push_str(&format!("\nAuthor Username: {}\n", username));
        }
        if let Some(author_id) = issue.get_metadata::<i64>("author_id") {
            context.push_str(&format!("Author ID: {}\n", author_id));
        }
        Ok(context)
    }
}

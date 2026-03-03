//! Telegram webhook handler.

use super::WebhookHandler;
use crate::ask_reply_inbox;
use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use claudear_config::config::TelegramConfig;
use claudear_core::error::Result;
use claudear_core::types::{Issue, MatchPriority, MatchResult};
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
        let secret = std::env::var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET").ok();
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

#[cfg(test)]
mod tests {
    use super::*;
    use claudear_config::config::TelegramConfig;
    use std::sync::Mutex;

    /// Guard to serialize tests that manipulate `CLAUDEAR_TELEGRAM_WEBHOOK_SECRET`.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_handler(listen_chat_id: Option<&str>) -> TelegramWebhookHandler {
        TelegramWebhookHandler::new(TelegramConfig {
            listen_chat_id: listen_chat_id.map(|s| s.to_string()),
            ..Default::default()
        })
    }

    #[test]
    fn test_source_name() {
        let handler = make_handler(None);
        assert_eq!(handler.source_name(), "telegram");
    }

    #[test]
    fn test_listen_chat_id_from_listen_field() {
        let handler = make_handler(Some("-1001234567890"));
        assert_eq!(handler.listen_chat_id(), Some(-1001234567890));
    }

    #[test]
    fn test_listen_chat_id_falls_back_to_chat_id() {
        let handler = TelegramWebhookHandler::new(TelegramConfig {
            chat_id: Some("-1001234567890".to_string()),
            ..Default::default()
        });
        assert_eq!(handler.listen_chat_id(), Some(-1001234567890));
    }

    #[test]
    fn test_listen_chat_id_none() {
        let handler = make_handler(None);
        assert_eq!(handler.listen_chat_id(), None);
    }

    #[test]
    fn test_listen_chat_id_invalid_parse() {
        let handler = make_handler(Some("not-a-number"));
        assert_eq!(handler.listen_chat_id(), None);
    }

    #[test]
    fn test_message_url_supergroup() {
        let url = TelegramWebhookHandler::message_url(-1001234567890, 42);
        assert_eq!(url, "https://t.me/c/1234567890/42");
    }

    #[test]
    fn test_message_url_regular_chat() {
        let url = TelegramWebhookHandler::message_url(12345, 42);
        assert_eq!(url, "");
    }

    #[test]
    fn test_extract_title_short() {
        assert_eq!(
            TelegramWebhookHandler::extract_title("Short title"),
            "Short title"
        );
    }

    #[test]
    fn test_extract_title_multiline() {
        assert_eq!(
            TelegramWebhookHandler::extract_title("First\nSecond"),
            "First"
        );
    }

    #[test]
    fn test_extract_title_long() {
        let long = "a".repeat(200);
        let title = TelegramWebhookHandler::extract_title(&long);
        assert!(title.len() <= 103);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_verify_signature_no_env_secret() {
        let _lock = ENV_LOCK.lock().unwrap();
        // When no CLAUDEAR_TELEGRAM_WEBHOOK_SECRET env, should accept any request
        std::env::remove_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET");
        let handler = make_handler(None);
        assert!(handler.verify_signature(b"any", &HashMap::new()));
    }

    #[test]
    fn test_verify_signature_with_env_secret_missing_header() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET", "mysecret");
        let handler = make_handler(None);
        assert!(!handler.verify_signature(b"any", &HashMap::new()));
        std::env::remove_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET");
    }

    #[test]
    fn test_verify_signature_with_env_secret_correct() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET", "mysecret");
        let handler = make_handler(None);
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".to_string(),
            "mysecret".to_string(),
        );
        assert!(handler.verify_signature(b"any", &headers));
        std::env::remove_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET");
    }

    #[test]
    fn test_verify_signature_with_env_secret_wrong() {
        let _lock = ENV_LOCK.lock().unwrap();
        std::env::set_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET", "mysecret");
        let handler = make_handler(None);
        let mut headers = HashMap::new();
        headers.insert(
            "x-telegram-bot-api-secret-token".to_string(),
            "wrong".to_string(),
        );
        assert!(!handler.verify_signature(b"any", &headers));
        std::env::remove_var("CLAUDEAR_TELEGRAM_WEBHOOK_SECRET");
    }

    #[tokio::test]
    async fn test_parse_payload_no_message() {
        let handler = make_handler(None);
        let payload = serde_json::json!({"update_id": 123});
        assert!(handler.parse_payload(&payload).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_parse_payload_wrong_chat_id() {
        let handler = make_handler(Some("-1001111111111"));
        let payload = serde_json::json!({
            "message": {
                "message_id": 1,
                "chat": {"id": -1002222222222_i64},
                "from": {"id": 100, "is_bot": false},
                "text": "hello"
            }
        });
        assert!(handler.parse_payload(&payload).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_parse_payload_from_bot() {
        let handler = make_handler(None);
        let payload = serde_json::json!({
            "message": {
                "message_id": 1,
                "chat": {"id": 123},
                "from": {"id": 100, "is_bot": true},
                "text": "hello"
            }
        });
        assert!(handler.parse_payload(&payload).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_parse_payload_empty_text() {
        let handler = make_handler(None);
        let payload = serde_json::json!({
            "message": {
                "message_id": 1,
                "chat": {"id": 123},
                "from": {"id": 100, "is_bot": false},
                "text": "  "
            }
        });
        assert!(handler.parse_payload(&payload).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_parse_payload_no_text() {
        let handler = make_handler(None);
        let payload = serde_json::json!({
            "message": {
                "message_id": 1,
                "chat": {"id": 123},
                "from": {"id": 100, "is_bot": false}
            }
        });
        assert!(handler.parse_payload(&payload).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_parse_payload_valid_message() {
        let handler = make_handler(None);
        let payload = serde_json::json!({
            "message": {
                "message_id": 42,
                "chat": {"id": -1001234567890_i64},
                "from": {"id": 100, "is_bot": false, "username": "testuser"},
                "text": "Bug: something broke",
                "date": 1700000000
            }
        });
        let issue = handler.parse_payload(&payload).await.unwrap().unwrap();
        assert_eq!(issue.source, "telegram");
        assert_eq!(issue.short_id, "TG-42");
        assert_eq!(issue.title, "Bug: something broke");
        assert_eq!(
            issue.get_metadata::<i64>("chat_id").unwrap(),
            -1001234567890
        );
        assert_eq!(issue.get_metadata::<i64>("message_id").unwrap(), 42);
        assert_eq!(issue.get_metadata::<i64>("author_id").unwrap(), 100);
        assert_eq!(
            issue.get_metadata::<String>("author_username").unwrap(),
            "testuser"
        );
    }

    #[tokio::test]
    async fn test_parse_payload_valid_no_username() {
        let handler = make_handler(None);
        let payload = serde_json::json!({
            "message": {
                "message_id": 10,
                "chat": {"id": 123},
                "from": {"id": 200, "is_bot": false},
                "text": "hello world"
            }
        });
        let issue = handler.parse_payload(&payload).await.unwrap().unwrap();
        assert!(issue.get_metadata::<String>("author_username").is_none());
    }

    #[test]
    fn test_matches_criteria() {
        let handler = make_handler(None);
        let issue = Issue::new("1", "TG-1", "title", "url", "telegram");
        assert!(handler.matches_criteria(&issue).matches);
    }

    #[tokio::test]
    async fn test_build_issue_context() {
        let handler = make_handler(None);
        let mut issue = Issue::new("1", "TG-1", "Bug title", "url", "telegram");
        issue.description = Some("Details here".to_string());
        issue.set_metadata("author_username", "bob");
        issue.set_metadata("author_id", 42i64);
        let ctx = handler.build_issue_context(&issue).await.unwrap();
        assert!(ctx.contains("Bug title"));
        assert!(ctx.contains("Details here"));
        assert!(ctx.contains("bob"));
        assert!(ctx.contains("42"));
    }
}

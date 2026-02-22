//! Telegram message source adapter.
//!
//! Polls a Telegram chat for human messages using the Bot API `getUpdates`
//! long-polling endpoint with cursor-based offset tracking.

use super::IssueSource;
use crate::config::TelegramConfig;
use crate::error::{Error, Result};
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::RwLock;

// ---------------------------------------------------------------------------
// Telegram Bot API response types
// ---------------------------------------------------------------------------

/// Top-level response envelope from the Telegram Bot API.
#[derive(Debug, Deserialize)]
struct TelegramResponse {
    ok: bool,
    #[serde(default)]
    result: Vec<TelegramUpdate>,
}

/// A single update from `getUpdates`.
#[derive(Debug, Clone, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    #[serde(default)]
    message: Option<TelegramMessage>,
}

/// A Telegram message object.
#[derive(Debug, Clone, Deserialize)]
struct TelegramMessage {
    message_id: i64,
    #[serde(default)]
    from: Option<TelegramUser>,
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
    date: i64,
}

/// A Telegram user (message sender).
#[derive(Debug, Clone, Deserialize)]
struct TelegramUser {
    id: i64,
    is_bot: bool,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    first_name: Option<String>,
}

/// A Telegram chat (group, supergroup, or private).
#[derive(Debug, Clone, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(default)]
    title: Option<String>,
}

/// Response from the `sendMessage` API.
#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    ok: bool,
    #[serde(default)]
    result: Option<TelegramMessage>,
}

// ---------------------------------------------------------------------------
// TelegramSource
// ---------------------------------------------------------------------------

/// Telegram chat polling source that converts messages into issues.
pub struct TelegramSource {
    config: TelegramConfig,
    /// Last seen update_id for cursor-based polling. `None` means first poll (seed).
    last_update_id: RwLock<Option<i64>>,
    /// Reusable HTTP client.
    client: reqwest::Client,
    /// Cache of recent messages for `get_issue` lookups, keyed by message_id string.
    cache: RwLock<HashMap<String, TelegramMessage>>,
}

impl TelegramSource {
    /// Create a new Telegram source from config.
    pub fn new(config: TelegramConfig) -> Self {
        Self {
            config,
            last_update_id: RwLock::new(None),
            client: reqwest::Client::new(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get the chat ID to listen on (listen_chat_id falls back to chat_id).
    fn listen_chat_id(&self) -> Option<&str> {
        self.config
            .listen_chat_id
            .as_deref()
            .or(self.config.chat_id.as_deref())
    }

    /// Build the base URL for Bot API calls.
    fn api_url(&self, method: &str) -> std::result::Result<String, Error> {
        let token = self
            .config
            .bot_token
            .as_ref()
            .map(|s| s.expose())
            .ok_or_else(|| Error::config("Telegram bot_token is required for source polling"))?;
        Ok(format!("https://api.telegram.org/bot{}/{}", token, method))
    }

    /// Extract a title from message text (first line, max 100 chars).
    fn extract_title(text: &str) -> String {
        let first_line = text.lines().next().unwrap_or(text);
        if first_line.len() > 100 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(97)])
        } else {
            first_line.to_string()
        }
    }

    /// Build a Telegram deep-link URL for a message.
    ///
    /// For supergroups (chat_id < -1_000_000_000_000) we strip the `-100` prefix
    /// to form `https://t.me/c/{channel_id}/{message_id}`.
    /// For other chat types there is no stable public URL, so we return an empty string.
    fn message_url(chat_id: i64, message_id: i64) -> String {
        // Supergroup IDs start at -1001000000000 in the Bot API.
        if chat_id < -1_000_000_000_000 {
            // Strip the -100 prefix: e.g. -1001234567890 -> 1234567890
            let channel_id = (-chat_id) - 1_000_000_000_000;
            format!("https://t.me/c/{}/{}", channel_id, message_id)
        } else {
            String::new()
        }
    }

    /// Convert a Telegram message into an `Issue`.
    fn message_to_issue(msg: &TelegramMessage) -> Issue {
        let text = msg.text.as_deref().unwrap_or("");
        let short_id = format!("TG-{}", msg.message_id);
        let title = Self::extract_title(text);
        let url = Self::message_url(msg.chat.id, msg.message_id);

        let mut issue = Issue::new(
            msg.message_id.to_string(),
            &short_id,
            &title,
            &url,
            "telegram",
        );
        issue.description = Some(text.to_string());

        if let Some(ref user) = msg.from {
            if let Some(ref username) = user.username {
                issue.set_metadata("author_username", username);
            }
            issue.set_metadata("author_id", &user.id.to_string());
        }
        issue.set_metadata("chat_id", &msg.chat.id.to_string());
        issue.set_metadata("message_id", &msg.message_id.to_string());

        issue
    }

    /// Cache a message for later `get_issue` lookups.
    fn cache_message(&self, msg: &TelegramMessage) {
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.insert(msg.message_id.to_string(), msg.clone());
    }
}

#[async_trait]
impl IssueSource for TelegramSource {
    fn name(&self) -> &str {
        "telegram"
    }

    fn display_name(&self) -> &str {
        "Telegram Messages"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let last_id = self
            .last_update_id
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // Build getUpdates URL with optional offset.
        let mut url = self.api_url("getUpdates")?;
        url.push_str("?timeout=0");
        if let Some(id) = last_id {
            url.push_str(&format!("&offset={}", id + 1));
        }

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Error::source("telegram", format!("getUpdates request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::source(
                "telegram",
                format!("getUpdates returned {}: {}", status, body),
            ));
        }

        let api_resp: TelegramResponse = resp
            .json()
            .await
            .map_err(|e| Error::source("telegram", format!("Failed to parse response: {}", e)))?;

        if !api_resp.ok {
            return Err(Error::source(
                "telegram",
                "Telegram API returned ok=false",
            ));
        }

        let updates = api_resp.result;

        if updates.is_empty() {
            return Ok(vec![]);
        }

        // Update cursor to the latest update_id.
        if let Some(latest) = updates.last() {
            let mut lock = self
                .last_update_id
                .write()
                .unwrap_or_else(|e| e.into_inner());
            *lock = Some(latest.update_id);
        }

        // First poll (cursor was None): seed and return empty.
        if last_id.is_none() {
            tracing::info!(
                update_id = ?updates.last().map(|u| u.update_id),
                "Telegram source seeded cursor"
            );
            return Ok(vec![]);
        }

        // Resolve listen chat filter.
        let listen_chat: Option<i64> = self
            .listen_chat_id()
            .and_then(|s| s.parse::<i64>().ok());

        let issues: Vec<Issue> = updates
            .iter()
            .filter_map(|u| u.message.as_ref())
            // Skip bot messages.
            .filter(|msg| {
                msg.from
                    .as_ref()
                    .map_or(true, |user| !user.is_bot)
            })
            // Skip messages without text.
            .filter(|msg| {
                msg.text
                    .as_ref()
                    .map_or(false, |t| !t.trim().is_empty())
            })
            // Filter to listen_chat_id if configured.
            .filter(|msg| {
                listen_chat.map_or(true, |cid| msg.chat.id == cid)
            })
            .map(|msg| {
                self.cache_message(msg);
                Self::message_to_issue(msg)
            })
            .collect();

        if !issues.is_empty() {
            tracing::info!(
                count = issues.len(),
                "Telegram source fetched new issues"
            );
        }

        Ok(issues)
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        // All Telegram messages that pass filtering are valid issues.
        MatchResult::matched("telegram_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("Telegram Message Issue: {}\n", issue.title);

        if let Some(ref desc) = issue.description {
            context.push_str(&format!("\nMessage:\n{}\n", desc));
        }

        if let Some(author) = issue.get_metadata::<String>("author_username") {
            context.push_str(&format!("\nAuthor: @{}\n", author));
        }

        if let Some(chat_id) = issue.get_metadata::<String>("chat_id") {
            context.push_str(&format!("Chat ID: {}\n", chat_id));
        }

        if !issue.url.is_empty() {
            context.push_str(&format!("\nURL: {}\n", issue.url));
        }

        Ok(context)
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
        let msg = cache
            .get(issue_id)
            .ok_or_else(|| Error::issue_not_found("telegram", issue_id))?;
        Ok(Self::message_to_issue(msg))
    }

    async fn create_issue(
        &self,
        title: &str,
        description: &str,
        _labels: &[String],
    ) -> Result<Issue> {
        let chat_id = self
            .listen_chat_id()
            .ok_or_else(|| Error::config("Telegram chat_id is required to create an issue"))?
            .to_string();

        let content = if description.is_empty() {
            title.to_string()
        } else {
            format!("{}\n\n{}", title, description)
        };

        let url = self.api_url("sendMessage")?;

        let resp = self
            .client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": content,
            }))
            .send()
            .await
            .map_err(|e| {
                Error::Other(format!("Failed to send Telegram message: {}", e))
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "Telegram sendMessage returned {}: {}",
                status, body
            )));
        }

        let api_resp: SendMessageResponse = resp.json().await.map_err(|e| {
            Error::Other(format!(
                "Failed to parse Telegram sendMessage response: {}",
                e
            ))
        })?;

        if !api_resp.ok {
            return Err(Error::Other(
                "Telegram sendMessage returned ok=false".into(),
            ));
        }

        let msg = api_resp.result.ok_or_else(|| {
            Error::Other("Telegram sendMessage response missing result".into())
        })?;

        self.cache_message(&msg);
        Ok(Self::message_to_issue(&msg))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretValue;

    fn make_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: Some(SecretValue::new("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11")),
            chat_id: Some("-1001234567890".to_string()),
            to_chat_ids: vec![],
            source_enabled: true,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn make_message(message_id: i64, text: &str, is_bot: bool) -> TelegramMessage {
        TelegramMessage {
            message_id,
            from: Some(TelegramUser {
                id: 42,
                is_bot,
                username: Some("testuser".to_string()),
                first_name: Some("Test".to_string()),
            }),
            chat: TelegramChat {
                id: -1001234567890,
                title: Some("Test Group".to_string()),
            },
            text: Some(text.to_string()),
            date: 1700000000,
        }
    }

    fn make_update(update_id: i64, message: TelegramMessage) -> TelegramUpdate {
        TelegramUpdate {
            update_id,
            message: Some(message),
        }
    }

    // -----------------------------------------------------------------------
    // Deserialization tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_deserialize_telegram_response_ok() {
        let json = r#"{
            "ok": true,
            "result": [
                {
                    "update_id": 100,
                    "message": {
                        "message_id": 1,
                        "from": {"id": 42, "is_bot": false, "username": "alice"},
                        "chat": {"id": -1001234567890, "title": "Dev"},
                        "text": "hello world",
                        "date": 1700000000
                    }
                }
            ]
        }"#;
        let resp: TelegramResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result.len(), 1);
        assert_eq!(resp.result[0].update_id, 100);
        let msg = resp.result[0].message.as_ref().unwrap();
        assert_eq!(msg.message_id, 1);
        assert_eq!(msg.text.as_deref(), Some("hello world"));
        assert_eq!(msg.chat.id, -1001234567890);
        assert_eq!(msg.chat.title.as_deref(), Some("Dev"));
    }

    #[test]
    fn test_deserialize_telegram_response_empty() {
        let json = r#"{"ok": true, "result": []}"#;
        let resp: TelegramResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(resp.result.is_empty());
    }

    #[test]
    fn test_deserialize_update_without_message() {
        let json = r#"{"update_id": 200}"#;
        let update: TelegramUpdate = serde_json::from_str(json).unwrap();
        assert_eq!(update.update_id, 200);
        assert!(update.message.is_none());
    }

    #[test]
    fn test_deserialize_message_without_optional_fields() {
        let json = r#"{
            "message_id": 5,
            "chat": {"id": 999},
            "date": 1700000000
        }"#;
        let msg: TelegramMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.message_id, 5);
        assert!(msg.from.is_none());
        assert!(msg.text.is_none());
        assert!(msg.chat.title.is_none());
    }

    #[test]
    fn test_deserialize_user_without_optional_fields() {
        let json = r#"{"id": 10, "is_bot": false}"#;
        let user: TelegramUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.id, 10);
        assert!(!user.is_bot);
        assert!(user.username.is_none());
        assert!(user.first_name.is_none());
    }

    #[test]
    fn test_deserialize_send_message_response() {
        let json = r#"{
            "ok": true,
            "result": {
                "message_id": 77,
                "chat": {"id": -1001234567890, "title": "Chat"},
                "date": 1700000000
            }
        }"#;
        let resp: SendMessageResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(resp.result.is_some());
        assert_eq!(resp.result.unwrap().message_id, 77);
    }

    // -----------------------------------------------------------------------
    // extract_title tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_title_short() {
        assert_eq!(TelegramSource::extract_title("Short title"), "Short title");
    }

    #[test]
    fn test_extract_title_multiline() {
        assert_eq!(
            TelegramSource::extract_title("First line\nSecond line\nThird"),
            "First line"
        );
    }

    #[test]
    fn test_extract_title_long() {
        let long = "a".repeat(150);
        let title = TelegramSource::extract_title(&long);
        assert_eq!(title.len(), 100);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_extract_title_empty() {
        assert_eq!(TelegramSource::extract_title(""), "");
    }

    // -----------------------------------------------------------------------
    // message_url tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_message_url_supergroup() {
        let url = TelegramSource::message_url(-1001234567890, 42);
        assert_eq!(url, "https://t.me/c/1234567890/42");
    }

    #[test]
    fn test_message_url_regular_chat() {
        let url = TelegramSource::message_url(123456, 7);
        assert_eq!(url, "");
    }

    #[test]
    fn test_message_url_negative_group() {
        // Regular group chats (not supergroups) have IDs > -1_000_000_000_000.
        let url = TelegramSource::message_url(-987654, 10);
        assert_eq!(url, "");
    }

    // -----------------------------------------------------------------------
    // message_to_issue tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_message_to_issue_basic() {
        let msg = make_message(42, "Fix the login bug\nMore details", false);
        let issue = TelegramSource::message_to_issue(&msg);

        assert_eq!(issue.id, "42");
        assert_eq!(issue.short_id, "TG-42");
        assert_eq!(issue.title, "Fix the login bug");
        assert_eq!(issue.source, "telegram");
        assert_eq!(
            issue.description.as_deref(),
            Some("Fix the login bug\nMore details")
        );
        assert_eq!(issue.url, "https://t.me/c/1234567890/42");
        assert_eq!(
            issue.get_metadata::<String>("author_username"),
            Some("testuser".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("author_id"),
            Some("42".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("chat_id"),
            Some("-1001234567890".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("message_id"),
            Some("42".to_string())
        );
    }

    #[test]
    fn test_message_to_issue_no_author() {
        let msg = TelegramMessage {
            message_id: 99,
            from: None,
            chat: TelegramChat {
                id: -1001234567890,
                title: Some("Chat".to_string()),
            },
            text: Some("Anonymous message".to_string()),
            date: 1700000000,
        };
        let issue = TelegramSource::message_to_issue(&msg);
        assert_eq!(issue.id, "99");
        assert!(issue.get_metadata::<String>("author_username").is_none());
        assert!(issue.get_metadata::<String>("author_id").is_none());
    }

    #[test]
    fn test_message_to_issue_no_text() {
        let msg = TelegramMessage {
            message_id: 50,
            from: None,
            chat: TelegramChat {
                id: 100,
                title: None,
            },
            text: None,
            date: 1700000000,
        };
        let issue = TelegramSource::message_to_issue(&msg);
        assert_eq!(issue.title, "");
        assert_eq!(issue.description.as_deref(), Some(""));
    }

    // -----------------------------------------------------------------------
    // listen_chat_id tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_listen_chat_id_fallback() {
        let source = TelegramSource::new(make_config());
        assert_eq!(source.listen_chat_id(), Some("-1001234567890"));
    }

    #[test]
    fn test_listen_chat_id_explicit() {
        let mut config = make_config();
        config.listen_chat_id = Some("-1009999999999".to_string());
        let source = TelegramSource::new(config);
        assert_eq!(source.listen_chat_id(), Some("-1009999999999"));
    }

    #[test]
    fn test_listen_chat_id_none() {
        let mut config = make_config();
        config.chat_id = None;
        config.listen_chat_id = None;
        let source = TelegramSource::new(config);
        assert!(source.listen_chat_id().is_none());
    }

    // -----------------------------------------------------------------------
    // name / display_name tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_name_and_display_name() {
        let source = TelegramSource::new(make_config());
        assert_eq!(source.name(), "telegram");
        assert_eq!(source.display_name(), "Telegram Messages");
    }

    // -----------------------------------------------------------------------
    // matches_criteria tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_matches_criteria_always() {
        let source = TelegramSource::new(make_config());
        let issue = Issue::new("1", "TG-1", "Test", "http://test.com", "telegram");
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.reason, "telegram_message");
    }

    // -----------------------------------------------------------------------
    // build_issue_context tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_build_issue_context_full() {
        let source = TelegramSource::new(make_config());
        let mut issue = Issue::new(
            "1",
            "TG-1",
            "Fix login",
            "https://t.me/c/1234567890/1",
            "telegram",
        );
        issue.description = Some("Fix the login bug please".to_string());
        issue.set_metadata("author_username", "alice");
        issue.set_metadata("chat_id", "-1001234567890");

        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Fix login"));
        assert!(context.contains("Fix the login bug please"));
        assert!(context.contains("@alice"));
        assert!(context.contains("-1001234567890"));
        assert!(context.contains("https://t.me/c/1234567890/1"));
    }

    #[tokio::test]
    async fn test_build_issue_context_minimal() {
        let source = TelegramSource::new(make_config());
        let issue = Issue::new("1", "TG-1", "Test", "", "telegram");
        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Test"));
        // No URL section when url is empty.
        assert!(!context.contains("URL:"));
    }

    // -----------------------------------------------------------------------
    // get_issue tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_issue_from_cache() {
        let source = TelegramSource::new(make_config());
        let msg = make_message(42, "Cached message", false);
        source.cache_message(&msg);

        let issue = source.get_issue("42").await.unwrap();
        assert_eq!(issue.id, "42");
        assert_eq!(issue.short_id, "TG-42");
        assert_eq!(issue.description.as_deref(), Some("Cached message"));
    }

    #[tokio::test]
    async fn test_get_issue_not_found() {
        let source = TelegramSource::new(make_config());
        let result = source.get_issue("999").await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, Error::IssueNotFound { .. }));
        assert!(err.to_string().contains("telegram"));
        assert!(err.to_string().contains("999"));
    }

    // -----------------------------------------------------------------------
    // Filtering logic tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_bot_messages_filtered() {
        // Verify the filter predicate used in fetch_issues: bot messages are skipped.
        let bot_msg = make_message(1, "bot says hi", true);
        let human_msg = make_message(2, "human says hi", false);

        let is_bot = |msg: &TelegramMessage| {
            msg.from.as_ref().map_or(false, |u| u.is_bot)
        };

        assert!(is_bot(&bot_msg));
        assert!(!is_bot(&human_msg));
    }

    #[test]
    fn test_empty_text_filtered() {
        let empty_msg = make_message(1, "   ", false);
        let valid_msg = make_message(2, "hello", false);

        let has_text = |msg: &TelegramMessage| {
            msg.text.as_ref().map_or(false, |t| !t.trim().is_empty())
        };

        assert!(!has_text(&empty_msg));
        assert!(has_text(&valid_msg));
    }

    #[test]
    fn test_no_text_field_filtered() {
        let msg = TelegramMessage {
            message_id: 1,
            from: Some(TelegramUser {
                id: 42,
                is_bot: false,
                username: None,
                first_name: None,
            }),
            chat: TelegramChat {
                id: -1001234567890,
                title: None,
            },
            text: None,
            date: 1700000000,
        };

        let has_text = |msg: &TelegramMessage| {
            msg.text.as_ref().map_or(false, |t| !t.trim().is_empty())
        };

        assert!(!has_text(&msg));
    }

    #[test]
    fn test_chat_id_filter() {
        let msg_right = make_message(1, "hello", false); // chat_id = -1001234567890
        let msg_wrong = TelegramMessage {
            message_id: 2,
            from: Some(TelegramUser {
                id: 42,
                is_bot: false,
                username: None,
                first_name: None,
            }),
            chat: TelegramChat {
                id: -1009999999999,
                title: None,
            },
            text: Some("hello".to_string()),
            date: 1700000000,
        };

        let listen: Option<i64> = Some(-1001234567890);
        let matches_chat = |msg: &TelegramMessage| {
            listen.map_or(true, |cid| msg.chat.id == cid)
        };

        assert!(matches_chat(&msg_right));
        assert!(!matches_chat(&msg_wrong));
    }

    #[test]
    fn test_no_chat_filter_passes_all() {
        let msg = make_message(1, "hello", false);
        let listen: Option<i64> = None;
        let matches_chat = |msg: &TelegramMessage| {
            listen.map_or(true, |cid| msg.chat.id == cid)
        };
        assert!(matches_chat(&msg));
    }

    // -----------------------------------------------------------------------
    // Seed / cursor tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_seed_behavior_initial_state() {
        let source = TelegramSource::new(make_config());
        assert!(source.last_update_id.read().unwrap().is_none());
    }

    #[test]
    fn test_cache_message_and_retrieve() {
        let source = TelegramSource::new(make_config());
        let msg = make_message(77, "cached", false);
        source.cache_message(&msg);

        let cache = source.cache.read().unwrap();
        assert!(cache.contains_key("77"));
        let cached = &cache["77"];
        assert_eq!(cached.message_id, 77);
        assert_eq!(cached.text.as_deref(), Some("cached"));
    }

    #[test]
    fn test_cache_overwrite() {
        let source = TelegramSource::new(make_config());
        let msg1 = make_message(1, "first", false);
        let msg2 = make_message(1, "second", false);
        source.cache_message(&msg1);
        source.cache_message(&msg2);

        let cache = source.cache.read().unwrap();
        assert_eq!(cache.len(), 1);
        assert_eq!(cache["1"].text.as_deref(), Some("second"));
    }

    // -----------------------------------------------------------------------
    // api_url tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_api_url_success() {
        let source = TelegramSource::new(make_config());
        let url = source.api_url("getUpdates").unwrap();
        assert_eq!(
            url,
            "https://api.telegram.org/bot123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11/getUpdates"
        );
    }

    #[test]
    fn test_api_url_no_token() {
        let mut config = make_config();
        config.bot_token = None;
        let source = TelegramSource::new(config);
        let result = source.api_url("getUpdates");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // create_issue error paths (no network)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_create_issue_no_chat_id() {
        let mut config = make_config();
        config.chat_id = None;
        config.listen_chat_id = None;
        let source = TelegramSource::new(config);

        let result = source.create_issue("title", "desc", &[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("chat_id"));
    }

    #[tokio::test]
    async fn test_create_issue_no_token() {
        let mut config = make_config();
        config.bot_token = None;
        let source = TelegramSource::new(config);

        let result = source.create_issue("title", "desc", &[]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("bot_token"));
    }
}

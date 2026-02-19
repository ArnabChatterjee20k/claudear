//! Slack message source adapter.
//!
//! Polls a Slack channel for human messages and converts them into issues.

use super::IssueSource;
use crate::config::SlackConfig;
use crate::error::{Error, Result};
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::RwLock;

/// Slack API response for conversations.history.
#[derive(Debug, Deserialize)]
struct SlackHistoryResponse {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    messages: Vec<SlackMessage>,
}

/// A Slack message from the API.
#[derive(Debug, Deserialize)]
struct SlackMessage {
    /// Slack timestamp (unique message ID), e.g. "1709123456.789012".
    ts: String,
    /// Message text.
    #[serde(default)]
    text: String,
    /// User ID of the sender (absent for bot messages posted via webhooks).
    user: Option<String>,
    /// Bot ID (present if sent by a bot).
    bot_id: Option<String>,
    /// Channel ID (may not be present in history responses).
    #[allow(dead_code)]
    #[serde(default)]
    channel: Option<String>,
}

/// Slack channel polling source that converts messages into issues.
pub struct SlackSource {
    config: SlackConfig,
    /// Last seen timestamp for incremental polling. `None` means first poll (seed).
    last_seen_ts: RwLock<Option<String>>,
    /// Reusable HTTP client.
    client: reqwest::Client,
}

impl SlackSource {
    /// Create a new Slack source from config.
    pub fn new(config: SlackConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            last_seen_ts: RwLock::new(None),
            client,
        }
    }

    /// Get the channel ID to listen on (listen_channel_id or fallback to channel_id).
    fn listen_channel_id(&self) -> Option<&str> {
        self.config
            .listen_channel_id
            .as_deref()
            .or(self.config.channel_id.as_deref())
    }

    /// Check if a message is from a bot.
    fn is_bot_message(msg: &SlackMessage) -> bool {
        msg.bot_id.is_some()
    }

    /// Extract a title from message content (first line, max 100 chars).
    fn extract_title(content: &str) -> String {
        let first_line = content.lines().next().unwrap_or(content);
        if first_line.len() > 100 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(97)])
        } else {
            first_line.to_string()
        }
    }

    /// Build a Slack message URL.
    /// Format: https://{workspace}.slack.com/archives/{channel}/p{ts_without_dots}
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

    /// Convert a Slack message to an Issue.
    fn message_to_issue(&self, msg: &SlackMessage, channel_id: &str) -> Issue {
        let short_id = format!("SLACK-{}", msg.ts.chars().take(8).collect::<String>());
        let title = Self::extract_title(&msg.text);
        let url = self.message_url(channel_id, &msg.ts);

        let mut issue = Issue::new(&msg.ts, &short_id, &title, &url, "slack");
        issue.description = Some(msg.text.clone());

        if let Some(ref user_id) = msg.user {
            issue.set_metadata("author_id", user_id);
        }
        issue.set_metadata("channel_id", channel_id);
        issue.set_metadata("message_ts", &msg.ts);

        issue
    }

    /// Fetch messages from Slack conversations.history.
    async fn fetch_history(
        &self,
        channel_id: &str,
        oldest: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SlackMessage>> {
        let bot_token = self
            .config
            .bot_token
            .as_deref()
            .ok_or_else(|| Error::config("Slack bot_token is required for source polling"))?;

        let mut url = format!(
            "https://slack.com/api/conversations.history?channel={}&limit={}",
            channel_id, limit
        );
        if let Some(oldest_ts) = oldest {
            url.push_str(&format!("&oldest={}", oldest_ts));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", bot_token))
            .send()
            .await?;

        let body = response.text().await.unwrap_or_default();
        let parsed: SlackHistoryResponse = serde_json::from_str(&body).map_err(|e| {
            Error::notifier(
                "slack_source",
                format!("Failed to parse Slack response: {}", e),
            )
        })?;

        if !parsed.ok {
            return Err(Error::notifier(
                "slack_source",
                format!(
                    "Slack API error: {}",
                    parsed.error.unwrap_or_else(|| "unknown".to_string())
                ),
            ));
        }

        Ok(parsed.messages)
    }
}

#[async_trait]
impl IssueSource for SlackSource {
    fn name(&self) -> &str {
        "slack"
    }

    fn display_name(&self) -> &str {
        "Slack Messages"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let channel_id = self
            .listen_channel_id()
            .ok_or_else(|| {
                Error::config(
                    "Slack listen_channel_id or channel_id is required for source polling",
                )
            })?
            .to_string();

        let last_seen = self
            .last_seen_ts
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        match last_seen {
            None => {
                // First poll: seed the cursor with the latest message, return no issues
                let messages = self.fetch_history(&channel_id, None, 1).await?;
                if let Some(latest) = messages.first() {
                    let mut lock = self.last_seen_ts.write().unwrap_or_else(|e| e.into_inner());
                    *lock = Some(latest.ts.clone());
                    tracing::info!(
                        message_ts = %latest.ts,
                        "Slack source seeded cursor"
                    );
                }
                Ok(vec![])
            }
            Some(oldest_ts) => {
                let messages = self
                    .fetch_history(&channel_id, Some(&oldest_ts), 100)
                    .await?;

                if messages.is_empty() {
                    return Ok(vec![]);
                }

                // Update cursor to the latest message
                // Slack returns messages newest-first by default
                if let Some(latest) = messages.first() {
                    let mut lock = self.last_seen_ts.write().unwrap_or_else(|e| e.into_inner());
                    *lock = Some(latest.ts.clone());
                }

                // Filter out bot messages and empty content, convert to issues
                let issues: Vec<Issue> = messages
                    .iter()
                    .filter(|msg| !Self::is_bot_message(msg))
                    .filter(|msg| !msg.text.trim().is_empty())
                    .map(|msg| self.message_to_issue(msg, &channel_id))
                    .collect();

                if !issues.is_empty() {
                    tracing::info!(
                        count = issues.len(),
                        channel_id = %channel_id,
                        "Slack source fetched new issues"
                    );
                }

                Ok(issues)
            }
        }
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        // All Slack messages that pass filtering are valid issues
        MatchResult::matched("slack_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("Slack Message Issue: {}\n", issue.title);

        if let Some(ref desc) = issue.description {
            context.push_str(&format!("\nMessage:\n{}\n", desc));
        }

        if let Some(author_id) = issue.get_metadata::<String>("author_id") {
            context.push_str(&format!("\nAuthor ID: {}\n", author_id));
        }

        context.push_str(&format!("\nURL: {}\n", issue.url));

        Ok(context)
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        Err(Error::issue_not_found("slack", issue_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_config() -> SlackConfig {
        SlackConfig {
            bot_token: Some("xoxb-test-token".to_string()),
            channel_id: Some("C12345".to_string()),
            source_enabled: true,
            listen_channel_id: None,
            workspace: Some("myworkspace".to_string()),
            ..Default::default()
        }
    }

    fn make_message(ts: &str, text: &str, bot: bool) -> SlackMessage {
        SlackMessage {
            ts: ts.to_string(),
            text: text.to_string(),
            user: if bot {
                None
            } else {
                Some("U12345".to_string())
            },
            bot_id: if bot {
                Some("B12345".to_string())
            } else {
                None
            },
            channel: Some("C12345".to_string()),
        }
    }

    #[test]
    fn test_extract_title_short() {
        assert_eq!(SlackSource::extract_title("Short title"), "Short title");
    }

    #[test]
    fn test_extract_title_multiline() {
        assert_eq!(
            SlackSource::extract_title("First line\nSecond line\nThird"),
            "First line"
        );
    }

    #[test]
    fn test_extract_title_long() {
        let long = "a".repeat(150);
        let title = SlackSource::extract_title(&long);
        assert_eq!(title.len(), 100);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_is_bot_message() {
        let bot_msg = make_message("1.0", "hello", true);
        let human_msg = make_message("2.0", "hello", false);
        assert!(SlackSource::is_bot_message(&bot_msg));
        assert!(!SlackSource::is_bot_message(&human_msg));
    }

    #[test]
    fn test_message_url_with_workspace() {
        let source = SlackSource::new(make_config());
        let url = source.message_url("C12345", "1709123456.789012");
        assert_eq!(
            url,
            "https://myworkspace.slack.com/archives/C12345/p1709123456789012"
        );
    }

    #[test]
    fn test_message_url_without_workspace() {
        let mut config = make_config();
        config.workspace = None;
        let source = SlackSource::new(config);
        let url = source.message_url("C12345", "1709123456.789012");
        assert_eq!(url, "https://slack.com/archives/C12345/p1709123456789012");
    }

    #[test]
    fn test_message_to_issue() {
        let source = SlackSource::new(make_config());
        let msg = make_message(
            "17091234.789012",
            "Fix the login bug\nMore details here",
            false,
        );
        let issue = source.message_to_issue(&msg, "C12345");

        assert_eq!(issue.id, "17091234.789012");
        assert_eq!(issue.short_id, "SLACK-17091234");
        assert_eq!(issue.title, "Fix the login bug");
        assert_eq!(issue.source, "slack");
        assert_eq!(
            issue.description.as_deref(),
            Some("Fix the login bug\nMore details here")
        );
        assert_eq!(
            issue.get_metadata::<String>("author_id"),
            Some("U12345".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("channel_id"),
            Some("C12345".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("message_ts"),
            Some("17091234.789012".to_string())
        );
    }

    #[test]
    fn test_listen_channel_id_fallback() {
        let source = SlackSource::new(make_config());
        assert_eq!(source.listen_channel_id(), Some("C12345"));
    }

    #[test]
    fn test_listen_channel_id_explicit() {
        let mut config = make_config();
        config.listen_channel_id = Some("C99999".to_string());
        let source = SlackSource::new(config);
        assert_eq!(source.listen_channel_id(), Some("C99999"));
    }

    #[test]
    fn test_name_and_display_name() {
        let source = SlackSource::new(make_config());
        assert_eq!(source.name(), "slack");
        assert_eq!(source.display_name(), "Slack Messages");
    }

    #[test]
    fn test_matches_criteria() {
        let source = SlackSource::new(make_config());
        let issue = Issue::new("1", "S-1", "Test", "http://test.com", "slack");
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[tokio::test]
    async fn test_build_issue_context() {
        let source = SlackSource::new(make_config());
        let mut issue = Issue::new(
            "1",
            "SLACK-1",
            "Fix login",
            "https://myworkspace.slack.com/archives/C12345/p1",
            "slack",
        );
        issue.description = Some("Fix the login bug please".to_string());
        issue.set_metadata("author_id", "U12345");

        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Fix login"));
        assert!(context.contains("Fix the login bug please"));
        assert!(context.contains("U12345"));
        assert!(context.contains("https://myworkspace.slack.com/archives/C12345/p1"));
    }

    #[tokio::test]
    async fn test_get_issue_returns_not_found() {
        let source = SlackSource::new(make_config());
        let result = source.get_issue("123").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_seed_behavior_initial_state() {
        let source = SlackSource::new(make_config());
        assert!(source.last_seen_ts.read().unwrap().is_none());
    }

    // ---------------------------------------------------------------
    // last_seen_ts RwLock state management
    // ---------------------------------------------------------------

    #[test]
    fn test_last_seen_ts_write_and_read() {
        let source = SlackSource::new(make_config());
        // Initially None
        assert!(source.last_seen_ts.read().unwrap().is_none());

        // Write a value
        {
            let mut lock = source.last_seen_ts.write().unwrap();
            *lock = Some("1709123456.789012".to_string());
        }

        // Read it back
        let ts = source.last_seen_ts.read().unwrap().clone();
        assert_eq!(ts, Some("1709123456.789012".to_string()));
    }

    #[test]
    fn test_last_seen_ts_overwrite() {
        let source = SlackSource::new(make_config());
        {
            let mut lock = source.last_seen_ts.write().unwrap();
            *lock = Some("1.0".to_string());
        }
        {
            let mut lock = source.last_seen_ts.write().unwrap();
            *lock = Some("2.0".to_string());
        }
        let ts = source.last_seen_ts.read().unwrap().clone();
        assert_eq!(ts, Some("2.0".to_string()));
    }

    #[test]
    fn test_last_seen_ts_multiple_reads_concurrent() {
        let source = SlackSource::new(make_config());
        {
            let mut lock = source.last_seen_ts.write().unwrap();
            *lock = Some("42.0".to_string());
        }
        // Multiple concurrent reads should not block each other
        let r1 = source.last_seen_ts.read().unwrap();
        let r2 = source.last_seen_ts.read().unwrap();
        assert_eq!(*r1, Some("42.0".to_string()));
        assert_eq!(*r2, Some("42.0".to_string()));
    }

    // ---------------------------------------------------------------
    // resolve_issue and add_comment (default trait no-ops)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_resolve_issue_returns_ok() {
        let source = SlackSource::new(make_config());
        let result = source.resolve_issue("some-issue-id").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_comment_returns_ok() {
        let source = SlackSource::new(make_config());
        let result = source.add_comment("some-issue-id", "a comment").await;
        assert!(result.is_ok());
    }

    // ---------------------------------------------------------------
    // is_terminal_status (default trait impl)
    // ---------------------------------------------------------------

    #[test]
    fn test_is_terminal_status_terminal_values() {
        let source = SlackSource::new(make_config());
        assert!(source.is_terminal_status("completed"));
        assert!(source.is_terminal_status("resolved"));
        assert!(source.is_terminal_status("cancelled"));
        assert!(source.is_terminal_status("canceled"));
        assert!(source.is_terminal_status("ignored"));
        assert!(source.is_terminal_status("closed"));
        assert!(source.is_terminal_status("done"));
    }

    #[test]
    fn test_is_terminal_status_case_insensitive() {
        let source = SlackSource::new(make_config());
        assert!(source.is_terminal_status("Completed"));
        assert!(source.is_terminal_status("RESOLVED"));
        assert!(source.is_terminal_status("Cancelled"));
        assert!(source.is_terminal_status("DONE"));
        assert!(source.is_terminal_status("Closed"));
    }

    #[test]
    fn test_is_terminal_status_non_terminal() {
        let source = SlackSource::new(make_config());
        assert!(!source.is_terminal_status("open"));
        assert!(!source.is_terminal_status("in_progress"));
        assert!(!source.is_terminal_status("pending"));
        assert!(!source.is_terminal_status("new"));
        assert!(!source.is_terminal_status(""));
        assert!(!source.is_terminal_status("something_random"));
    }

    // ---------------------------------------------------------------
    // get_issue_status (default trait impl delegates to get_issue)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_get_issue_status_propagates_not_found() {
        let source = SlackSource::new(make_config());
        // get_issue_status calls get_issue which returns IssueNotFound
        let result = source.get_issue_status("nonexistent").await;
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // get_issue error details
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_get_issue_error_contains_issue_id() {
        let source = SlackSource::new(make_config());
        let result = source.get_issue("SLACK-999").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("SLACK-999") || err_msg.contains("slack"),
            "Error should reference the issue ID or source: {}",
            err_msg
        );
    }

    // ---------------------------------------------------------------
    // extract_title edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_extract_title_empty_string() {
        assert_eq!(SlackSource::extract_title(""), "");
    }

    #[test]
    fn test_extract_title_exactly_100_chars() {
        let s = "a".repeat(100);
        let title = SlackSource::extract_title(&s);
        assert_eq!(title.len(), 100);
        assert!(!title.ends_with("..."));
    }

    #[test]
    fn test_extract_title_101_chars_gets_truncated() {
        let s = "a".repeat(101);
        let title = SlackSource::extract_title(&s);
        assert_eq!(title.len(), 100);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_extract_title_unicode_boundary() {
        // Use multibyte characters to ensure floor_char_boundary works correctly
        // Each emoji is 4 bytes. 25 emojis = 100 bytes but only 25 chars.
        // With more than 100 bytes in the first line, truncation kicks in.
        let s = "\u{1F600}".repeat(30); // 30 emojis = 120 bytes
        let title = SlackSource::extract_title(&s);
        // Should end with "..." and not panic on char boundary
        assert!(title.ends_with("..."));
        assert!(title.len() <= 100);
    }

    #[test]
    fn test_extract_title_whitespace_only_first_line() {
        assert_eq!(SlackSource::extract_title("   \nSecond line"), "   ");
    }

    #[test]
    fn test_extract_title_newline_only() {
        // lines() yields empty string for "\n"
        assert_eq!(SlackSource::extract_title("\n"), "");
    }

    // ---------------------------------------------------------------
    // message_to_issue edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_message_to_issue_bot_message_no_author() {
        let source = SlackSource::new(make_config());
        let msg = make_message("1.0", "Bot says hello", true);
        let issue = source.message_to_issue(&msg, "C12345");

        // Bot messages have no user field, so author_id should not be set
        assert_eq!(issue.get_metadata::<String>("author_id"), None);
        assert_eq!(issue.source, "slack");
    }

    #[test]
    fn test_message_to_issue_empty_text() {
        let source = SlackSource::new(make_config());
        let msg = make_message("1.0", "", false);
        let issue = source.message_to_issue(&msg, "C12345");

        assert_eq!(issue.title, "");
        assert_eq!(issue.description, Some("".to_string()));
    }

    #[test]
    fn test_message_to_issue_short_id_format() {
        let source = SlackSource::new(make_config());
        // Timestamp with 8+ chars before take(8) truncation
        let msg = make_message("17091234.789012", "test", false);
        let issue = source.message_to_issue(&msg, "C12345");
        assert_eq!(issue.short_id, "SLACK-17091234");

        // Timestamp with fewer than 8 chars
        let msg2 = make_message("123", "test", false);
        let issue2 = source.message_to_issue(&msg2, "C12345");
        assert_eq!(issue2.short_id, "SLACK-123");
    }

    #[test]
    fn test_message_to_issue_url_included() {
        let source = SlackSource::new(make_config());
        let msg = make_message("1709123456.789012", "test", false);
        let issue = source.message_to_issue(&msg, "C99999");
        assert_eq!(
            issue.url,
            "https://myworkspace.slack.com/archives/C99999/p1709123456789012"
        );
    }

    // ---------------------------------------------------------------
    // build_issue_context edge cases
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_build_issue_context_no_description() {
        let source = SlackSource::new(make_config());
        let issue = Issue::new("1", "S-1", "Title only", "http://example.com", "slack");
        // No description, no author_id
        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Title only"));
        assert!(context.contains("http://example.com"));
        assert!(!context.contains("Message:"));
        assert!(!context.contains("Author ID:"));
    }

    #[tokio::test]
    async fn test_build_issue_context_with_description_no_author() {
        let source = SlackSource::new(make_config());
        let mut issue = Issue::new("1", "S-1", "Title", "http://example.com", "slack");
        issue.description = Some("Some description text".to_string());
        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Message:"));
        assert!(context.contains("Some description text"));
        assert!(!context.contains("Author ID:"));
    }

    #[tokio::test]
    async fn test_build_issue_context_with_author_no_description() {
        let source = SlackSource::new(make_config());
        let mut issue = Issue::new("1", "S-1", "Title", "http://example.com", "slack");
        issue.set_metadata("author_id", "UABC123");
        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Author ID: UABC123"));
        assert!(!context.contains("Message:"));
    }

    // ---------------------------------------------------------------
    // matches_criteria details
    // ---------------------------------------------------------------

    #[test]
    fn test_matches_criteria_returns_normal_priority() {
        let source = SlackSource::new(make_config());
        let issue = Issue::new("1", "S-1", "Test", "http://test.com", "slack");
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
        assert_eq!(result.reason, "slack_message");
    }

    #[test]
    fn test_matches_criteria_ignores_issue_content() {
        let source = SlackSource::new(make_config());
        // Criteria matching is unconditional for Slack -- any issue matches
        let issue1 = Issue::new("1", "S-1", "", "http://t.com", "other_source");
        let issue2 = Issue::new("2", "S-2", "Very important bug", "http://t.com", "slack");
        assert!(source.matches_criteria(&issue1).matches);
        assert!(source.matches_criteria(&issue2).matches);
    }

    // ---------------------------------------------------------------
    // listen_channel_id edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_listen_channel_id_both_none() {
        let mut config = make_config();
        config.channel_id = None;
        config.listen_channel_id = None;
        let source = SlackSource::new(config);
        assert_eq!(source.listen_channel_id(), None);
    }

    #[test]
    fn test_listen_channel_id_prefers_listen_over_channel() {
        let mut config = make_config();
        config.channel_id = Some("C_FALLBACK".to_string());
        config.listen_channel_id = Some("C_LISTEN".to_string());
        let source = SlackSource::new(config);
        assert_eq!(source.listen_channel_id(), Some("C_LISTEN"));
    }

    // ---------------------------------------------------------------
    // message_url edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_message_url_timestamp_no_dot() {
        let source = SlackSource::new(make_config());
        let url = source.message_url("C111", "12345");
        assert_eq!(url, "https://myworkspace.slack.com/archives/C111/p12345");
    }

    #[test]
    fn test_message_url_timestamp_multiple_dots() {
        let source = SlackSource::new(make_config());
        let url = source.message_url("C111", "1.2.3");
        assert_eq!(url, "https://myworkspace.slack.com/archives/C111/p123");
    }

    // ---------------------------------------------------------------
    // is_bot_message edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_is_bot_message_no_bot_id_no_user() {
        // A message with neither user nor bot_id (edge case)
        let msg = SlackMessage {
            ts: "1.0".to_string(),
            text: "ghost".to_string(),
            user: None,
            bot_id: None,
            channel: None,
        };
        assert!(!SlackSource::is_bot_message(&msg));
    }

    #[test]
    fn test_is_bot_message_both_user_and_bot_id() {
        // A message with both user and bot_id (apps can do this)
        let msg = SlackMessage {
            ts: "1.0".to_string(),
            text: "app msg".to_string(),
            user: Some("U123".to_string()),
            bot_id: Some("B456".to_string()),
            channel: None,
        };
        // Should be classified as bot because bot_id is present
        assert!(SlackSource::is_bot_message(&msg));
    }

    // ---------------------------------------------------------------
    // SlackHistoryResponse deserialization
    // ---------------------------------------------------------------

    #[test]
    fn test_history_response_deserialize_ok() {
        let json = r#"{"ok":true,"messages":[{"ts":"1.0","text":"hello","user":"U1"}]}"#;
        let resp: SlackHistoryResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.messages.len(), 1);
        assert_eq!(resp.messages[0].text, "hello");
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_history_response_deserialize_error() {
        let json = r#"{"ok":false,"error":"channel_not_found"}"#;
        let resp: SlackHistoryResponse = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.error, Some("channel_not_found".to_string()));
        assert!(resp.messages.is_empty());
    }

    #[test]
    fn test_history_response_deserialize_missing_fields_defaults() {
        let json = r#"{"ok":true}"#;
        let resp: SlackHistoryResponse = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert!(resp.messages.is_empty());
        assert!(resp.error.is_none());
    }

    #[test]
    fn test_slack_message_deserialize_minimal() {
        let json = r#"{"ts":"1709.0","text":"hi"}"#;
        let msg: SlackMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, "1709.0");
        assert_eq!(msg.text, "hi");
        assert!(msg.user.is_none());
        assert!(msg.bot_id.is_none());
        assert!(msg.channel.is_none());
    }

    #[test]
    fn test_slack_message_deserialize_defaults_on_missing_text() {
        let json = r#"{"ts":"1.0"}"#;
        let msg: SlackMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.text, ""); // #[serde(default)]
    }
}

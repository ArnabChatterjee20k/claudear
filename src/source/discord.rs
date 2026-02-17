//! Discord message source adapter.
//!
//! Polls a Discord channel for human messages and converts them into issues.

use super::IssueSource;
use crate::config::DiscordConfig;
use crate::discord::{DiscordClient, DiscordMessage};
use crate::error::{Error, Result};
use crate::types::{Issue, MatchPriority, MatchResult};
use async_trait::async_trait;
use std::sync::RwLock;

/// Discord channel polling source that converts messages into issues.
pub struct DiscordSource {
    config: DiscordConfig,
    /// Last seen message ID for incremental polling. `None` means first poll (seed).
    last_seen_id: RwLock<Option<String>>,
    /// Reusable Discord API client, created once during construction.
    /// `None` when bot_token is not configured.
    client: Option<DiscordClient>,
}

impl DiscordSource {
    /// Create a new Discord source from config.
    pub fn new(config: DiscordConfig) -> Self {
        let client = config
            .bot_token
            .as_deref()
            .and_then(|token| DiscordClient::new(token).ok());
        Self {
            config,
            last_seen_id: RwLock::new(None),
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
    fn is_bot_message(msg: &DiscordMessage) -> bool {
        msg.author.as_ref().is_some_and(|a| a.bot)
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

    /// Build a Discord message URL.
    fn message_url(&self, channel_id: &str, message_id: &str) -> String {
        match &self.config.guild_id {
            Some(guild_id) => format!(
                "https://discord.com/channels/{}/{}/{}",
                guild_id, channel_id, message_id
            ),
            None => format!(
                "https://discord.com/channels/@me/{}/{}",
                channel_id, message_id
            ),
        }
    }

    /// Convert a Discord message to an Issue.
    fn message_to_issue(&self, msg: &DiscordMessage) -> Issue {
        let short_id = format!("DISCORD-{}", msg.id.chars().take(8).collect::<String>());
        let title = Self::extract_title(&msg.content);
        let url = self.message_url(&msg.channel_id, &msg.id);

        let mut issue = Issue::new(&msg.id, &short_id, &title, &url, "discord");
        issue.description = Some(msg.content.clone());

        if let Some(ref author) = msg.author {
            issue.set_metadata("author_username", &author.username);
            issue.set_metadata("author_id", &author.id);
        }
        issue.set_metadata("channel_id", &msg.channel_id);

        issue
    }
}

#[async_trait]
impl IssueSource for DiscordSource {
    fn name(&self) -> &str {
        "discord"
    }

    fn display_name(&self) -> &str {
        "Discord Messages"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| Error::config("Discord bot_token is required for source polling"))?;

        let channel_id = self
            .listen_channel_id()
            .ok_or_else(|| {
                Error::config(
                    "Discord listen_channel_id or channel_id is required for source polling",
                )
            })?
            .to_string();

        let last_seen = self
            .last_seen_id
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        match last_seen {
            None => {
                // First poll: seed the cursor with the latest message, return no issues
                let messages = client.list_channel_messages(&channel_id, 1).await?;
                if let Some(latest) = messages.first() {
                    let mut lock = self.last_seen_id.write().unwrap_or_else(|e| e.into_inner());
                    *lock = Some(latest.id.clone());
                    tracing::info!(
                        message_id = %latest.id,
                        "Discord source seeded cursor"
                    );
                }
                Ok(vec![])
            }
            Some(after_id) => {
                let messages = client
                    .list_channel_messages_after(&channel_id, &after_id, 100)
                    .await?;

                if messages.is_empty() {
                    return Ok(vec![]);
                }

                // Update cursor to the latest message
                if let Some(latest) = messages.last() {
                    let mut lock = self.last_seen_id.write().unwrap_or_else(|e| e.into_inner());
                    *lock = Some(latest.id.clone());
                }

                // Filter out bot messages and empty content, convert to issues
                let issues: Vec<Issue> = messages
                    .iter()
                    .filter(|msg| !Self::is_bot_message(msg))
                    .filter(|msg| !msg.content.trim().is_empty())
                    .map(|msg| self.message_to_issue(msg))
                    .collect();

                if !issues.is_empty() {
                    tracing::info!(
                        count = issues.len(),
                        channel_id = %channel_id,
                        "Discord source fetched new issues"
                    );
                }

                Ok(issues)
            }
        }
    }

    fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
        // All Discord messages that pass filtering are valid issues
        MatchResult::matched("discord_message", MatchPriority::Normal)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("Discord Message Issue: {}\n", issue.title);

        if let Some(ref desc) = issue.description {
            context.push_str(&format!("\nMessage:\n{}\n", desc));
        }

        if let Some(author) = issue.get_metadata::<String>("author_username") {
            context.push_str(&format!("\nAuthor: {}\n", author));
        }

        context.push_str(&format!("\nURL: {}\n", issue.url));

        Ok(context)
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        Err(Error::issue_not_found("discord", issue_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::DiscordUser;

    fn make_config() -> DiscordConfig {
        DiscordConfig {
            bot_token: Some("test-token".to_string()),
            channel_id: Some("chan-123".to_string()),
            source_enabled: true,
            listen_channel_id: None,
            guild_id: Some("guild-456".to_string()),
            ..Default::default()
        }
    }

    fn make_message(id: &str, content: &str, bot: bool) -> DiscordMessage {
        DiscordMessage {
            id: id.to_string(),
            channel_id: "chan-123".to_string(),
            author: Some(DiscordUser {
                id: "user-1".to_string(),
                username: "testuser".to_string(),
                discriminator: "0".to_string(),
                avatar: None,
                bot,
            }),
            content: content.to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_reference: None,
            thread: None,
        }
    }

    #[test]
    fn test_extract_title_short() {
        assert_eq!(DiscordSource::extract_title("Short title"), "Short title");
    }

    #[test]
    fn test_extract_title_multiline() {
        assert_eq!(
            DiscordSource::extract_title("First line\nSecond line\nThird"),
            "First line"
        );
    }

    #[test]
    fn test_extract_title_long() {
        let long = "a".repeat(150);
        let title = DiscordSource::extract_title(&long);
        assert_eq!(title.len(), 100);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn test_is_bot_message() {
        let bot_msg = make_message("1", "hello", true);
        let human_msg = make_message("2", "hello", false);
        let no_author = DiscordMessage {
            id: "3".to_string(),
            channel_id: "c".to_string(),
            author: None,
            content: "hello".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            message_reference: None,
            thread: None,
        };

        assert!(DiscordSource::is_bot_message(&bot_msg));
        assert!(!DiscordSource::is_bot_message(&human_msg));
        assert!(!DiscordSource::is_bot_message(&no_author));
    }

    #[test]
    fn test_message_url_with_guild() {
        let source = DiscordSource::new(make_config());
        let url = source.message_url("chan-123", "msg-789");
        assert_eq!(
            url,
            "https://discord.com/channels/guild-456/chan-123/msg-789"
        );
    }

    #[test]
    fn test_message_url_without_guild() {
        let mut config = make_config();
        config.guild_id = None;
        let source = DiscordSource::new(config);
        let url = source.message_url("chan-123", "msg-789");
        assert_eq!(url, "https://discord.com/channels/@me/chan-123/msg-789");
    }

    #[test]
    fn test_message_to_issue() {
        let source = DiscordSource::new(make_config());
        let msg = make_message("123456789", "Fix the login bug\nMore details here", false);
        let issue = source.message_to_issue(&msg);

        assert_eq!(issue.id, "123456789");
        assert_eq!(issue.short_id, "DISCORD-12345678");
        assert_eq!(issue.title, "Fix the login bug");
        assert_eq!(issue.source, "discord");
        assert_eq!(
            issue.description.as_deref(),
            Some("Fix the login bug\nMore details here")
        );
        assert_eq!(
            issue.get_metadata::<String>("author_username"),
            Some("testuser".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("author_id"),
            Some("user-1".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("channel_id"),
            Some("chan-123".to_string())
        );
    }

    #[test]
    fn test_listen_channel_id_fallback() {
        let source = DiscordSource::new(make_config());
        assert_eq!(source.listen_channel_id(), Some("chan-123"));
    }

    #[test]
    fn test_listen_channel_id_explicit() {
        let mut config = make_config();
        config.listen_channel_id = Some("listen-789".to_string());
        let source = DiscordSource::new(config);
        assert_eq!(source.listen_channel_id(), Some("listen-789"));
    }

    #[test]
    fn test_name_and_display_name() {
        let source = DiscordSource::new(make_config());
        assert_eq!(source.name(), "discord");
        assert_eq!(source.display_name(), "Discord Messages");
    }

    #[test]
    fn test_matches_criteria() {
        let source = DiscordSource::new(make_config());
        let issue = Issue::new("1", "D-1", "Test", "http://test.com", "discord");
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[tokio::test]
    async fn test_build_issue_context() {
        let source = DiscordSource::new(make_config());
        let mut issue = Issue::new(
            "1",
            "DISCORD-1",
            "Fix login",
            "https://discord.com/channels/g/c/1",
            "discord",
        );
        issue.description = Some("Fix the login bug please".to_string());
        issue.set_metadata("author_username", "alice");

        let context = source.build_issue_context(&issue).await.unwrap();
        assert!(context.contains("Fix login"));
        assert!(context.contains("Fix the login bug please"));
        assert!(context.contains("alice"));
        assert!(context.contains("https://discord.com/channels/g/c/1"));
    }

    #[tokio::test]
    async fn test_get_issue_returns_not_found() {
        let source = DiscordSource::new(make_config());
        let result = source.get_issue("123").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_seed_behavior_initial_state() {
        let source = DiscordSource::new(make_config());
        assert!(source.last_seen_id.read().unwrap().is_none());
    }
}

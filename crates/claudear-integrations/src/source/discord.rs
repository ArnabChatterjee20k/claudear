//! Discord message source adapter.
//!
//! Polls a Discord channel for human messages and converts them into issues.

use super::IssueSource;
use crate::discord::{DiscordClient, DiscordMessage};
use async_trait::async_trait;
use claudear_config::config::DiscordConfig;
use claudear_core::error::{Error, Result};
use claudear_core::types::{Issue, MatchPriority, MatchResult};
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
            .as_ref()
            .map(|s| s.expose())
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

    /// Check if a message is from a bot (excludes webhook messages, which
    /// appear as bot-authored but are user-triggered via webhook URLs).
    fn is_bot_message(msg: &DiscordMessage) -> bool {
        if msg.webhook_id.is_some() {
            return false;
        }
        msg.author.as_ref().is_some_and(|a| a.bot)
    }

    /// Check if a message is one of our own notifications (e.g. ask questions,
    /// success/failure alerts). These are sent via webhook and should not be
    /// treated as new issues.
    fn is_own_notification(msg: &DiscordMessage) -> bool {
        // Ask question messages have an embed with "Input needed:" title.
        if msg.embeds.iter().any(|e| {
            e.title
                .as_ref()
                .is_some_and(|t| t.contains("Input needed:"))
        }) {
            return true;
        }
        // Webhook messages with Claudear embeds are our notifications.
        // Their text content is either empty or only user mentions (<@123>).
        if msg.webhook_id.is_some() {
            let stripped = Self::strip_mentions(&msg.content);
            if stripped.trim().is_empty() {
                return true;
            }
        }
        false
    }

    /// Whether `msg` @-mentions the given bot user id.
    ///
    /// Prefers Discord's parsed `mentions` array (authoritative — set by Discord
    /// only for genuine mentions), and falls back to scanning the raw content for
    /// the mention token `<@ID>` / `<@!ID>` in case `mentions` wasn't populated.
    fn mentions_bot(msg: &DiscordMessage, bot_id: &str) -> bool {
        if bot_id.is_empty() {
            return false;
        }
        if msg.mentions.iter().any(|u| u.id == bot_id) {
            return true;
        }
        msg.content.contains(&format!("<@{}>", bot_id))
            || msg.content.contains(&format!("<@!{}>", bot_id))
    }

    /// Whether a message is eligible for ingestion given the configured
    /// `bot_id` gate: if a `bot_id` is set, the message must @-mention the bot;
    /// otherwise every message qualifies (legacy "reply to all" behaviour).
    fn passes_mention_gate(&self, msg: &DiscordMessage) -> bool {
        match self.config.bot_id.as_deref() {
            Some(bot_id) if !bot_id.is_empty() => Self::mentions_bot(msg, bot_id),
            _ => true,
        }
    }

    /// Strip Discord user mentions (<@123>, <@!123>) from text.
    fn strip_mentions(content: &str) -> String {
        let mut result = content.to_string();
        while let Some(start) = result.find("<@") {
            if let Some(end) = result[start..].find('>') {
                result.replace_range(start..start + end + 1, "");
            } else {
                break;
            }
        }
        result
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
                    tracing::info!(message_id = %latest.id, "Discord source seeded cursor");
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

                // Filter out bot messages, our own notifications, and empty
                // content. When a bot_id is configured, also require the message
                // to @-mention the bot (engage only when tagged).
                let issues: Vec<Issue> = messages
                    .iter()
                    .filter(|msg| !Self::is_bot_message(msg))
                    .filter(|msg| !Self::is_own_notification(msg))
                    .filter(|msg| !msg.content.trim().is_empty())
                    .filter(|msg| self.passes_mention_gate(msg))
                    .map(|msg| self.message_to_issue(msg))
                    .collect();

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

    async fn create_issue(
        &self,
        title: &str,
        description: &str,
        _labels: &[String],
    ) -> Result<Issue> {
        let channel_id = self
            .listen_channel_id()
            .ok_or_else(|| Error::config("Discord channel_id is required to create an issue"))?
            .to_string();

        let content = if description.is_empty() {
            title.to_string()
        } else {
            format!("{}\n\n{}", title, description)
        };

        // Prefer webhook URL: messages posted via webhook have webhook_id set,
        // which bypasses the is_bot_message filter. This makes them appear as
        // user-posted messages to the daemon's poll_issues.
        if let Some(ref webhook_url) = self.config.webhook_url {
            let url = format!("{}?wait=true", webhook_url.expose());
            let http = reqwest::Client::new();
            let resp = http
                .post(&url)
                .json(&serde_json::json!({ "content": content }))
                .send()
                .await
                .map_err(|e| Error::Other(format!("Failed to post Discord webhook: {}", e)))?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(Error::Other(format!(
                    "Discord webhook returned {}: {}",
                    status, body
                )));
            }

            let msg: crate::discord::DiscordMessage = resp.json().await.map_err(|e| {
                Error::Other(format!("Failed to parse Discord webhook response: {}", e))
            })?;

            return Ok(self.message_to_issue(&msg));
        }

        // Fallback: use bot token to send message directly.
        let client = self.client.as_ref().ok_or_else(|| {
            Error::config("Discord bot_token or webhook_url is required to create an issue")
        })?;

        let params = crate::discord::CreateMessageParams::text(content);
        let msg = client
            .send_message(&channel_id, params)
            .await
            .map_err(|e| Error::Other(format!("Failed to create Discord issue: {}", e)))?;

        Ok(self.message_to_issue(&msg))
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        let client = self
            .client
            .as_ref()
            .ok_or_else(|| Error::config("Discord bot_token is required to fetch an issue"))?;

        let channel_id = self
            .listen_channel_id()
            .ok_or_else(|| Error::config("Discord channel_id is required to fetch an issue"))?
            .to_string();

        let msg = client
            .get_message(&channel_id, issue_id)
            .await
            .map_err(|_| Error::issue_not_found("discord", issue_id))?;

        Ok(self.message_to_issue(&msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::DiscordUser;

    fn make_config() -> DiscordConfig {
        DiscordConfig {
            bot_token: Some("test-token".into()),
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
            webhook_id: None,
            embeds: vec![],
            mentions: vec![],
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
            webhook_id: None,
            embeds: vec![],
            mentions: vec![],
        };
        // Webhook messages have author.bot=true but should NOT be filtered
        let mut webhook_msg = make_message("4", "hello", true);
        webhook_msg.webhook_id = Some("wh-123".to_string());

        assert!(DiscordSource::is_bot_message(&bot_msg));
        assert!(!DiscordSource::is_bot_message(&human_msg));
        assert!(!DiscordSource::is_bot_message(&no_author));
        assert!(!DiscordSource::is_bot_message(&webhook_msg));
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

    // --- Bot-mention gate ---

    #[test]
    fn test_mentions_bot_via_structured_mentions() {
        // The authoritative path: Discord's parsed `mentions` array contains
        // the bot, even if the raw content has no `<@id>` token.
        let mut msg = make_message("1", "how do I paginate?", false);
        msg.mentions = vec![DiscordUser {
            id: "999".to_string(),
            username: "claudear".to_string(),
            discriminator: "0".to_string(),
            avatar: None,
            bot: true,
        }];
        assert!(DiscordSource::mentions_bot(&msg, "999"));
        // A different user mentioned is not the bot.
        assert!(!DiscordSource::mentions_bot(&msg, "111"));
    }

    #[test]
    fn test_mentions_bot_content_fallback() {
        // Fallback when `mentions` wasn't populated: scan raw content for the
        // mention token in both forms.
        let plain = make_message("1", "hey <@999> how do I paginate?", false);
        let nick = make_message("2", "<@!999> help", false);
        let other = make_message("3", "hey <@111> look", false);
        let none = make_message("4", "how do I paginate?", false);

        assert!(DiscordSource::mentions_bot(&plain, "999"));
        assert!(DiscordSource::mentions_bot(&nick, "999"));
        assert!(!DiscordSource::mentions_bot(&other, "999"));
        assert!(!DiscordSource::mentions_bot(&none, "999"));
        // Empty bot id never matches.
        assert!(!DiscordSource::mentions_bot(&plain, ""));
    }

    #[test]
    fn test_passes_mention_gate_requires_tag_when_bot_id_set() {
        let mut config = make_config();
        config.bot_id = Some("999".to_string());
        let source = DiscordSource::new(config);

        let tagged = make_message("1", "<@999> what is query not equal syntax?", false);
        let untagged = make_message("2", "what is query not equal syntax?", false);

        assert!(source.passes_mention_gate(&tagged));
        assert!(!source.passes_mention_gate(&untagged));
    }

    #[test]
    fn test_passes_mention_gate_allows_all_when_bot_id_unset() {
        // Default config has no bot_id → legacy "reply to all" behaviour.
        let source = DiscordSource::new(make_config());
        let untagged = make_message("1", "no mention here", false);
        assert!(source.passes_mention_gate(&untagged));
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
    async fn test_get_issue_no_client_returns_error() {
        let mut config = make_config();
        config.bot_token = None;
        let source = DiscordSource::new(config);
        let result = source.get_issue("123").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_seed_behavior_initial_state() {
        let source = DiscordSource::new(make_config());
        assert!(source.last_seen_id.read().unwrap().is_none());
    }
}

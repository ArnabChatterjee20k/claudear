//! Discord API types for thread management.

use serde::{Deserialize, Serialize};

/// A Discord user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordUser {
    /// User ID.
    pub id: String,
    /// Username.
    pub username: String,
    /// Discriminator (legacy, may be 0).
    #[serde(default)]
    pub discriminator: String,
    /// Avatar hash.
    pub avatar: Option<String>,
    /// Whether the user is a bot.
    #[serde(default)]
    pub bot: bool,
}

/// A Discord channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordChannel {
    /// Channel ID.
    pub id: String,
    /// Channel type.
    #[serde(rename = "type")]
    pub channel_type: u8,
    /// Guild ID (if applicable).
    pub guild_id: Option<String>,
    /// Channel name.
    pub name: Option<String>,
    /// Parent channel ID (for threads).
    pub parent_id: Option<String>,
}

/// A Discord thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordThread {
    /// Thread channel ID.
    pub id: String,
    /// Thread type (11 = public, 12 = private).
    #[serde(rename = "type")]
    pub thread_type: u8,
    /// Guild ID.
    pub guild_id: Option<String>,
    /// Thread name.
    pub name: String,
    /// Parent channel ID.
    pub parent_id: Option<String>,
    /// Owner ID.
    pub owner_id: Option<String>,
    /// Whether the thread is archived.
    #[serde(default)]
    pub archived: bool,
    /// Whether the thread is locked.
    #[serde(default)]
    pub locked: bool,
    /// Message count.
    pub message_count: Option<u32>,
    /// Member count.
    pub member_count: Option<u32>,
}

impl DiscordThread {
    /// Check if the thread is still active (not archived or locked).
    pub fn is_active(&self) -> bool {
        !self.archived && !self.locked
    }
}

/// A Discord message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordMessage {
    /// Message ID.
    pub id: String,
    /// Channel ID.
    pub channel_id: String,
    /// Message author.
    pub author: Option<DiscordUser>,
    /// Message content.
    pub content: String,
    /// Timestamp.
    pub timestamp: String,
    /// Thread associated with this message (if started).
    pub thread: Option<DiscordThread>,
}

/// Parameters for creating a thread.
#[derive(Debug, Clone, Serialize)]
pub struct CreateThreadParams {
    /// Thread name (1-100 characters).
    pub name: String,
    /// Auto-archive duration in minutes (60, 1440, 4320, 10080).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_archive_duration: Option<u32>,
    /// Thread type (11 = public, 12 = private).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub thread_type: Option<u8>,
    /// Whether to send a rate limit error rather than being slow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_user: Option<u32>,
}

impl CreateThreadParams {
    /// Create params for a public thread.
    pub fn public(name: impl Into<String>) -> Self {
        let name = name.into();
        let name = if name.len() > 100 {
            name[..name.floor_char_boundary(100)].to_string()
        } else {
            name
        };
        Self {
            name,
            auto_archive_duration: Some(10080), // 7 days
            thread_type: Some(11),              // Public thread
            rate_limit_per_user: None,
        }
    }

    /// Create params for a private thread.
    pub fn private(name: impl Into<String>) -> Self {
        let name = name.into();
        let name = if name.len() > 100 {
            name[..name.floor_char_boundary(100)].to_string()
        } else {
            name
        };
        Self {
            name,
            auto_archive_duration: Some(10080), // 7 days
            thread_type: Some(12),              // Private thread
            rate_limit_per_user: None,
        }
    }

    /// Set auto-archive duration.
    pub fn with_auto_archive(mut self, minutes: u32) -> Self {
        self.auto_archive_duration = Some(minutes);
        self
    }
}

/// Parameters for creating a message.
#[derive(Debug, Clone, Serialize)]
pub struct CreateMessageParams {
    /// Message content (up to 2000 characters).
    pub content: String,
    /// Whether this is a TTS message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts: Option<bool>,
    /// Embeds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeds: Option<Vec<MessageEmbed>>,
}

impl CreateMessageParams {
    /// Create simple text message.
    pub fn text(content: impl Into<String>) -> Self {
        let content = content.into();
        let content = if content.len() > 2000 {
            format!("{}...", &content[..content.floor_char_boundary(1997)])
        } else {
            content
        };
        Self {
            content,
            tts: None,
            embeds: None,
        }
    }

    /// Create message with embed.
    pub fn with_embed(content: impl Into<String>, embed: MessageEmbed) -> Self {
        let content = content.into();
        let content = if content.len() > 2000 {
            format!("{}...", &content[..content.floor_char_boundary(1997)])
        } else {
            content
        };
        Self {
            content,
            tts: None,
            embeds: Some(vec![embed]),
        }
    }
}

/// A Discord embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEmbed {
    /// Embed title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Embed description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Embed URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Embed color.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    /// Embed fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<EmbedField>>,
    /// Footer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
    /// Timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl MessageEmbed {
    /// Create a simple embed.
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            url: None,
            color: None,
            fields: None,
            footer: None,
            timestamp: None,
        }
    }

    /// Set title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set URL.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Set color.
    pub fn color(mut self, color: u32) -> Self {
        self.color = Some(color);
        self
    }

    /// Add a field.
    pub fn field(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> Self {
        let field = EmbedField {
            name: name.into(),
            value: value.into(),
            inline: Some(inline),
        };
        self.fields.get_or_insert_with(Vec::new).push(field);
        self
    }

    /// Set footer.
    pub fn footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(EmbedFooter { text: text.into() });
        self
    }

    /// Set timestamp.
    pub fn timestamp(mut self, ts: impl Into<String>) -> Self {
        self.timestamp = Some(ts.into());
        self
    }
}

impl Default for MessageEmbed {
    fn default() -> Self {
        Self::new()
    }
}

/// An embed field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedField {
    /// Field name.
    pub name: String,
    /// Field value.
    pub value: String,
    /// Whether inline.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

/// An embed footer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedFooter {
    /// Footer text.
    pub text: String,
}

/// Thread state for tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadState {
    /// Thread ID.
    pub thread_id: String,
    /// Thread name.
    pub thread_name: String,
    /// Parent channel ID.
    pub channel_id: String,
    /// Associated PR URL.
    pub pr_url: String,
    /// Associated issue ID.
    pub issue_id: String,
    /// Issue source.
    pub source: String,
    /// Created timestamp.
    pub created_at: String,
    /// Whether the thread is still active.
    pub is_active: bool,
    /// Last message ID in thread.
    pub last_message_id: Option<String>,
}

impl ThreadState {
    /// Create a new thread state.
    pub fn new(
        thread_id: impl Into<String>,
        thread_name: impl Into<String>,
        channel_id: impl Into<String>,
        pr_url: impl Into<String>,
        issue_id: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            thread_id: thread_id.into(),
            thread_name: thread_name.into(),
            channel_id: channel_id.into(),
            pr_url: pr_url.into(),
            issue_id: issue_id.into(),
            source: source.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            is_active: true,
            last_message_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_thread_params_public() {
        let params = CreateThreadParams::public("Test Thread");
        assert_eq!(params.name, "Test Thread");
        assert_eq!(params.thread_type, Some(11));
        assert_eq!(params.auto_archive_duration, Some(10080));
    }

    #[test]
    fn test_create_thread_params_private() {
        let params = CreateThreadParams::private("Private Thread");
        assert_eq!(params.name, "Private Thread");
        assert_eq!(params.thread_type, Some(12));
    }

    #[test]
    fn test_create_thread_params_truncates_long_name() {
        let long_name = "a".repeat(150);
        let params = CreateThreadParams::public(&long_name);
        assert_eq!(params.name.len(), 100);
    }

    #[test]
    fn test_create_message_params_text() {
        let params = CreateMessageParams::text("Hello");
        assert_eq!(params.content, "Hello");
        assert!(params.embeds.is_none());
    }

    #[test]
    fn test_create_message_params_truncates() {
        let long_content = "a".repeat(3000);
        let params = CreateMessageParams::text(&long_content);
        assert_eq!(params.content.len(), 2000);
        assert!(params.content.ends_with("..."));
    }

    #[test]
    fn test_message_embed_builder() {
        let embed = MessageEmbed::new()
            .title("Test")
            .description("Desc")
            .color(0xFF0000)
            .field("Field1", "Value1", true)
            .footer("Footer");

        assert_eq!(embed.title, Some("Test".to_string()));
        assert_eq!(embed.description, Some("Desc".to_string()));
        assert_eq!(embed.color, Some(0xFF0000));
        assert!(embed.fields.is_some());
        assert_eq!(embed.fields.as_ref().unwrap().len(), 1);
        assert!(embed.footer.is_some());
    }

    #[test]
    fn test_thread_state_new() {
        let state = ThreadState::new(
            "123456",
            "PR: Fix Bug",
            "channel123",
            "https://github.com/user/repo/pull/1",
            "issue-1",
            "linear",
        );

        assert_eq!(state.thread_id, "123456");
        assert_eq!(state.thread_name, "PR: Fix Bug");
        assert!(state.is_active);
        assert!(state.last_message_id.is_none());
    }

    #[test]
    fn test_discord_thread_is_active() {
        let active_thread = DiscordThread {
            id: "123".to_string(),
            thread_type: 11,
            guild_id: None,
            name: "Test".to_string(),
            parent_id: None,
            owner_id: None,
            archived: false,
            locked: false,
            message_count: None,
            member_count: None,
        };
        assert!(active_thread.is_active());

        let archived_thread = DiscordThread {
            archived: true,
            ..active_thread.clone()
        };
        assert!(!archived_thread.is_active());

        let locked_thread = DiscordThread {
            locked: true,
            ..active_thread
        };
        assert!(!locked_thread.is_active());
    }

    #[test]
    fn test_create_thread_params_with_auto_archive() {
        let params = CreateThreadParams::public("Test").with_auto_archive(60);
        assert_eq!(params.auto_archive_duration, Some(60));
    }

    #[test]
    fn test_create_message_params_with_embed() {
        let embed = MessageEmbed::new().title("Title").description("Desc");
        let params = CreateMessageParams::with_embed("Message", embed);
        assert!(params.embeds.is_some());
        assert_eq!(params.embeds.unwrap().len(), 1);
    }

    #[test]
    fn test_message_embed_url() {
        let embed = MessageEmbed::new().url("https://example.com");
        assert_eq!(embed.url, Some("https://example.com".to_string()));
    }

    #[test]
    fn test_message_embed_timestamp() {
        let embed = MessageEmbed::new().timestamp("2024-01-01T00:00:00Z");
        assert_eq!(embed.timestamp, Some("2024-01-01T00:00:00Z".to_string()));
    }

    #[test]
    fn test_message_embed_multiple_fields() {
        let embed = MessageEmbed::new()
            .field("F1", "V1", true)
            .field("F2", "V2", false)
            .field("F3", "V3", true);
        let fields = embed.fields.unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "F1");
        assert!(fields[0].inline.unwrap());
        assert!(!fields[1].inline.unwrap());
    }

    #[test]
    fn test_thread_state_set_last_message() {
        let mut state = ThreadState::new("t1", "Thread", "c1", "https://pr", "i1", "linear");
        assert!(state.last_message_id.is_none());
        state.last_message_id = Some("msg123".to_string());
        assert_eq!(state.last_message_id.unwrap(), "msg123");
    }

    #[test]
    fn test_thread_state_deactivate() {
        let mut state = ThreadState::new("t1", "Thread", "c1", "https://pr", "i1", "linear");
        assert!(state.is_active);
        state.is_active = false;
        assert!(!state.is_active);
    }

    #[test]
    fn test_discord_user_serialization() {
        let user = DiscordUser {
            id: "123".to_string(),
            username: "testuser".to_string(),
            discriminator: "0001".to_string(),
            avatar: Some("abc123".to_string()),
            bot: false,
        };
        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("testuser"));
        assert!(json.contains("0001"));
    }

    #[test]
    fn test_discord_user_bot_flag() {
        let bot_user = DiscordUser {
            id: "456".to_string(),
            username: "bot".to_string(),
            discriminator: "0000".to_string(),
            avatar: None,
            bot: true,
        };
        assert!(bot_user.bot);
    }

    #[test]
    fn test_discord_channel_serialization() {
        let channel = DiscordChannel {
            id: "123".to_string(),
            channel_type: 0,
            guild_id: Some("guild123".to_string()),
            name: Some("general".to_string()),
            parent_id: None,
        };
        let json = serde_json::to_string(&channel).unwrap();
        assert!(json.contains("general"));
        assert!(json.contains("guild123"));
    }

    #[test]
    fn test_discord_message_serialization() {
        let message = DiscordMessage {
            id: "msg1".to_string(),
            channel_id: "chan1".to_string(),
            author: Some(DiscordUser {
                id: "user1".to_string(),
                username: "author".to_string(),
                discriminator: "0".to_string(),
                avatar: None,
                bot: false,
            }),
            content: "Hello world".to_string(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
            thread: None,
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("Hello world"));
        assert!(json.contains("author"));
    }

    #[test]
    fn test_embed_field_serialization() {
        let field = EmbedField {
            name: "Test".to_string(),
            value: "Value".to_string(),
            inline: Some(true),
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(json.contains("Test"));
        assert!(json.contains("Value"));
        assert!(json.contains("true"));
    }

    #[test]
    fn test_embed_footer_serialization() {
        let footer = EmbedFooter {
            text: "Footer text".to_string(),
        };
        let json = serde_json::to_string(&footer).unwrap();
        assert!(json.contains("Footer text"));
    }

    #[test]
    fn test_message_embed_default() {
        let embed = MessageEmbed::default();
        assert!(embed.title.is_none());
        assert!(embed.description.is_none());
        assert!(embed.color.is_none());
    }

    #[test]
    fn test_create_thread_params_exactly_100_chars() {
        let name = "a".repeat(100);
        let params = CreateThreadParams::public(&name);
        assert_eq!(params.name.len(), 100);
    }

    #[test]
    fn test_create_message_params_exactly_2000_chars() {
        let content = "a".repeat(2000);
        let params = CreateMessageParams::text(&content);
        assert_eq!(params.content.len(), 2000);
    }
}

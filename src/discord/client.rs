//! Discord API client for thread management.

use super::types::{
    CreateMessageParams, CreateThreadParams, DiscordChannel, DiscordMessage, DiscordThread,
};
use crate::error::{Error, Result};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// HTTP response abstraction for testability.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body)
            .map_err(|e| Error::notifier("discord", format!("Failed to parse response: {}", e)))
    }
}

/// Trait for HTTP client operations to enable testing.
#[async_trait]
pub trait DiscordHttpClient: Send + Sync {
    async fn get(&self, url: &str) -> Result<HttpResponse>;
    async fn post(&self, url: &str, body: serde_json::Value) -> Result<HttpResponse>;
    async fn patch(&self, url: &str, body: serde_json::Value) -> Result<HttpResponse>;
}

/// Default HTTP client using reqwest.
pub struct ReqwestDiscordClient {
    client: reqwest::Client,
}

impl ReqwestDiscordClient {
    pub fn new(bot_token: &str) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bot {}", bot_token))
                .map_err(|_| Error::config("Invalid bot token format"))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| Error::network(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self { client })
    }
}

#[async_trait]
impl DiscordHttpClient for ReqwestDiscordClient {
    async fn get(&self, url: &str) -> Result<HttpResponse> {
        let response = self.client.get(url).send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }

    async fn post(&self, url: &str, body: serde_json::Value) -> Result<HttpResponse> {
        let response = self.client.post(url).json(&body).send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }

    async fn patch(&self, url: &str, body: serde_json::Value) -> Result<HttpResponse> {
        let response = self.client.patch(url).json(&body).send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }
}

/// Discord API client for managing threads and messages.
pub struct DiscordClient<H: DiscordHttpClient = ReqwestDiscordClient> {
    http: H,
    bot_token: String,
}

impl DiscordClient<ReqwestDiscordClient> {
    /// Create a new Discord client with a bot token.
    pub fn new(bot_token: impl Into<String>) -> Result<Self> {
        let bot_token = bot_token.into();
        if bot_token.is_empty() {
            return Err(Error::config("DISCORD_BOT_TOKEN is required"));
        }

        let http = ReqwestDiscordClient::new(&bot_token)?;
        Ok(Self { http, bot_token })
    }
}

impl<H: DiscordHttpClient> DiscordClient<H> {
    /// Create a new Discord client with a custom HTTP client.
    pub fn with_http_client(bot_token: impl Into<String>, http: H) -> Result<Self> {
        let bot_token = bot_token.into();
        if bot_token.is_empty() {
            return Err(Error::config("DISCORD_BOT_TOKEN is required"));
        }
        Ok(Self { http, bot_token })
    }

    /// Get the bot token (for verification purposes).
    pub fn bot_token(&self) -> &str {
        &self.bot_token
    }

    /// Get a channel by ID.
    pub async fn get_channel(&self, channel_id: &str) -> Result<DiscordChannel> {
        let url = format!("{}/channels/{}", DISCORD_API_BASE, channel_id);
        let response = self.http.get(&url).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to get channel ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Create a thread in a channel (without a starting message).
    pub async fn create_thread(
        &self,
        channel_id: &str,
        params: CreateThreadParams,
    ) -> Result<DiscordThread> {
        let url = format!("{}/channels/{}/threads", DISCORD_API_BASE, channel_id);
        let body = serde_json::to_value(&params).map_err(|e| {
            Error::notifier("discord", format!("Failed to serialize params: {}", e))
        })?;
        let response = self.http.post(&url, body).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to create thread ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Create a thread from an existing message.
    pub async fn create_thread_from_message(
        &self,
        channel_id: &str,
        message_id: &str,
        params: CreateThreadParams,
    ) -> Result<DiscordThread> {
        let url = format!(
            "{}/channels/{}/messages/{}/threads",
            DISCORD_API_BASE, channel_id, message_id
        );
        let body = serde_json::to_value(&params).map_err(|e| {
            Error::notifier("discord", format!("Failed to serialize params: {}", e))
        })?;
        let response = self.http.post(&url, body).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to create thread from message ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Get a thread by ID.
    pub async fn get_thread(&self, thread_id: &str) -> Result<DiscordThread> {
        let url = format!("{}/channels/{}", DISCORD_API_BASE, thread_id);
        let response = self.http.get(&url).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to get thread ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Send a message to a channel or thread.
    pub async fn send_message(
        &self,
        channel_id: &str,
        params: CreateMessageParams,
    ) -> Result<DiscordMessage> {
        let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);
        let body = serde_json::to_value(&params).map_err(|e| {
            Error::notifier("discord", format!("Failed to serialize params: {}", e))
        })?;
        let response = self.http.post(&url, body).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to send message ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// List recent messages from a channel.
    pub async fn list_channel_messages(
        &self,
        channel_id: &str,
        limit: usize,
    ) -> Result<Vec<DiscordMessage>> {
        let clamped_limit = limit.clamp(1, 100);
        let url = format!(
            "{}/channels/{}/messages?limit={}",
            DISCORD_API_BASE, channel_id, clamped_limit
        );
        let response = self.http.get(&url).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to list channel messages ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Archive a thread.
    pub async fn archive_thread(&self, thread_id: &str) -> Result<DiscordThread> {
        let url = format!("{}/channels/{}", DISCORD_API_BASE, thread_id);
        let response = self
            .http
            .patch(&url, serde_json::json!({ "archived": true }))
            .await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to archive thread ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// Unarchive a thread.
    pub async fn unarchive_thread(&self, thread_id: &str) -> Result<DiscordThread> {
        let url = format!("{}/channels/{}", DISCORD_API_BASE, thread_id);
        let response = self
            .http
            .patch(&url, serde_json::json!({ "archived": false }))
            .await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to unarchive thread ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        response.json()
    }

    /// List active threads in a channel.
    pub async fn list_active_threads(&self, guild_id: &str) -> Result<Vec<DiscordThread>> {
        let url = format!("{}/guilds/{}/threads/active", DISCORD_API_BASE, guild_id);
        let response = self.http.get(&url).await?;

        if !response.is_success() {
            return Err(Error::notifier(
                "discord",
                format!(
                    "Failed to list threads ({}): {}",
                    response.status, response.body
                ),
            ));
        }

        #[derive(serde::Deserialize)]
        struct ThreadsResponse {
            threads: Vec<DiscordThread>,
        }

        let threads_response: ThreadsResponse = response.json()?;
        Ok(threads_response.threads)
    }
}

/// Mock HTTP client for testing (only available in tests).
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing.
    pub struct MockDiscordClient {
        get_responses: Mutex<HashMap<String, HttpResponse>>,
        post_responses: Mutex<HashMap<String, HttpResponse>>,
        patch_responses: Mutex<HashMap<String, HttpResponse>>,
    }

    impl MockDiscordClient {
        pub fn new() -> Self {
            Self {
                get_responses: Mutex::new(HashMap::new()),
                post_responses: Mutex::new(HashMap::new()),
                patch_responses: Mutex::new(HashMap::new()),
            }
        }

        pub fn mock_get(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            self.get_responses.lock().unwrap().insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        pub fn mock_post(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            self.post_responses.lock().unwrap().insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        pub fn mock_patch(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            self.patch_responses.lock().unwrap().insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }
    }

    #[async_trait]
    impl DiscordHttpClient for MockDiscordClient {
        async fn get(&self, url: &str) -> Result<HttpResponse> {
            let responses = self.get_responses.lock().unwrap();
            if let Some(r) = responses.get(url) {
                Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not found".to_string(),
                })
            }
        }

        async fn post(&self, url: &str, _body: serde_json::Value) -> Result<HttpResponse> {
            let responses = self.post_responses.lock().unwrap();
            if let Some(r) = responses.get(url) {
                Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not found".to_string(),
                })
            }
        }

        async fn patch(&self, url: &str, _body: serde_json::Value) -> Result<HttpResponse> {
            let responses = self.patch_responses.lock().unwrap();
            if let Some(r) = responses.get(url) {
                Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not found".to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::MockDiscordClient;
    use super::*;

    fn mock_channel_json() -> &'static str {
        r#"{"id": "123", "type": 0, "name": "test-channel"}"#
    }

    fn mock_thread_json() -> &'static str {
        r#"{"id": "456", "type": 11, "name": "test-thread", "parent_id": "123", "owner_id": "789"}"#
    }

    fn mock_message_json() -> &'static str {
        r#"{"id": "999", "channel_id": "123", "content": "Hello", "timestamp": "2024-01-01T00:00:00Z", "author": {"id": "111", "username": "bot"}}"#
    }

    #[test]
    fn test_http_response_is_success() {
        assert!(HttpResponse {
            status: 200,
            body: "".to_string()
        }
        .is_success());
        assert!(HttpResponse {
            status: 201,
            body: "".to_string()
        }
        .is_success());
        assert!(!HttpResponse {
            status: 400,
            body: "".to_string()
        }
        .is_success());
        assert!(!HttpResponse {
            status: 500,
            body: "".to_string()
        }
        .is_success());
    }

    #[test]
    fn test_http_response_json() {
        let response = HttpResponse {
            status: 200,
            body: r#"{"id": "123"}"#.to_string(),
        };
        let parsed: serde_json::Value = response.json().unwrap();
        assert_eq!(parsed["id"], "123");
    }

    #[test]
    fn test_http_response_json_error() {
        let response = HttpResponse {
            status: 200,
            body: "invalid".to_string(),
        };
        let result: Result<serde_json::Value> = response.json();
        assert!(result.is_err());
    }

    #[test]
    fn test_client_requires_token() {
        let result = DiscordClient::new("");
        assert!(result.is_err());
    }

    #[test]
    fn test_client_creation() {
        let result = DiscordClient::new("test_token");
        assert!(result.is_ok());
        let client = result.unwrap();
        assert_eq!(client.bot_token(), "test_token");
    }

    #[test]
    fn test_with_http_client_requires_token() {
        let mock = MockDiscordClient::new();
        let result = DiscordClient::with_http_client("", mock);
        assert!(result.is_err());
    }

    #[test]
    fn test_with_http_client_success() {
        let mock = MockDiscordClient::new();
        let result = DiscordClient::with_http_client("token", mock);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_channel_success() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/channels/123",
            200,
            mock_channel_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let channel = client.get_channel("123").await.unwrap();
        assert_eq!(channel.id, "123");
    }

    #[tokio::test]
    async fn test_get_channel_error() {
        let mock = MockDiscordClient::new();
        mock.mock_get("https://discord.com/api/v10/channels/123", 404, "Not found");

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let result = client.get_channel("123").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to get channel"));
    }

    #[tokio::test]
    async fn test_create_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/threads",
            200,
            mock_thread_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateThreadParams::public("Test Thread");
        let thread = client.create_thread("123", params).await.unwrap();
        assert_eq!(thread.id, "456");
    }

    #[tokio::test]
    async fn test_create_thread_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/threads",
            403,
            "Forbidden",
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateThreadParams::public("Test");
        let result = client.create_thread("123", params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_thread_from_message_success() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/messages/999/threads",
            200,
            mock_thread_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateThreadParams::public("Test");
        let thread = client
            .create_thread_from_message("123", "999", params)
            .await
            .unwrap();
        assert_eq!(thread.id, "456");
    }

    #[tokio::test]
    async fn test_create_thread_from_message_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/messages/999/threads",
            400,
            "Bad request",
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateThreadParams::public("Test");
        let result = client
            .create_thread_from_message("123", "999", params)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/channels/456",
            200,
            mock_thread_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let thread = client.get_thread("456").await.unwrap();
        assert_eq!(thread.id, "456");
    }

    #[tokio::test]
    async fn test_get_thread_error() {
        let mock = MockDiscordClient::new();
        mock.mock_get("https://discord.com/api/v10/channels/456", 404, "Not found");

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let result = client.get_thread("456").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_message_success() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/messages",
            200,
            mock_message_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateMessageParams::text("Hello");
        let message = client.send_message("123", params).await.unwrap();
        assert_eq!(message.id, "999");
    }

    #[tokio::test]
    async fn test_send_message_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/123/messages",
            403,
            "Forbidden",
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let params = CreateMessageParams::text("Hello");
        let result = client.send_message("123", params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_channel_messages_success() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/channels/123/messages?limit=10",
            200,
            &format!("[{}]", mock_message_json()),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let messages = client.list_channel_messages("123", 10).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "999");
    }

    #[tokio::test]
    async fn test_archive_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_patch(
            "https://discord.com/api/v10/channels/456",
            200,
            mock_thread_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let thread = client.archive_thread("456").await.unwrap();
        assert_eq!(thread.id, "456");
    }

    #[tokio::test]
    async fn test_archive_thread_error() {
        let mock = MockDiscordClient::new();
        mock.mock_patch("https://discord.com/api/v10/channels/456", 403, "Forbidden");

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let result = client.archive_thread("456").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_unarchive_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_patch(
            "https://discord.com/api/v10/channels/456",
            200,
            mock_thread_json(),
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let thread = client.unarchive_thread("456").await.unwrap();
        assert_eq!(thread.id, "456");
    }

    #[tokio::test]
    async fn test_unarchive_thread_error() {
        let mock = MockDiscordClient::new();
        mock.mock_patch(
            "https://discord.com/api/v10/channels/456",
            500,
            "Server error",
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let result = client.unarchive_thread("456").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_active_threads_success() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/guilds/guild1/threads/active",
            200,
            r#"{"threads": [{"id": "456", "type": 11, "name": "thread1", "parent_id": "123", "owner_id": "789"}]}"#,
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let threads = client.list_active_threads("guild1").await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "456");
    }

    #[tokio::test]
    async fn test_list_active_threads_error() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/guilds/guild1/threads/active",
            403,
            "Forbidden",
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let result = client.list_active_threads("guild1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_active_threads_empty() {
        let mock = MockDiscordClient::new();
        mock.mock_get(
            "https://discord.com/api/v10/guilds/guild1/threads/active",
            200,
            r#"{"threads": []}"#,
        );

        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let threads = client.list_active_threads("guild1").await.unwrap();
        assert!(threads.is_empty());
    }

    #[test]
    fn test_client_with_string() {
        let token = String::from("my_token");
        let client = DiscordClient::new(token).unwrap();
        assert_eq!(client.bot_token(), "my_token");
    }

    #[test]
    fn test_discord_api_base_url() {
        assert_eq!(DISCORD_API_BASE, "https://discord.com/api/v10");
    }

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
    fn test_create_message_params_text() {
        let params = CreateMessageParams::text("Hello");
        assert_eq!(params.content, "Hello".to_string());
        assert!(params.embeds.is_none());
    }

    #[test]
    fn test_create_message_params_with_embed() {
        let embed = super::super::types::MessageEmbed::new()
            .title("Test Title")
            .description("Test description");
        let params = CreateMessageParams::with_embed("Content", embed);
        assert_eq!(params.content, "Content".to_string());
        assert!(params.embeds.is_some());
    }
}

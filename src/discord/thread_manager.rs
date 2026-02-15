//! Thread manager for PR discussions.

use super::client::{DiscordClient, DiscordHttpClient, ReqwestDiscordClient};
use super::types::{CreateMessageParams, CreateThreadParams, MessageEmbed, ThreadState};
use crate::error::{Error, Result};
use crate::types::Issue;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Colors for different notification types.
mod colors {
    pub const SUCCESS: u32 = 0x2ecc71; // Green
    pub const ERROR: u32 = 0xe74c3c; // Red
    pub const INFO: u32 = 0x3498db; // Blue
    pub const WARNING: u32 = 0xf39c12; // Orange
    pub const PURPLE: u32 = 0x9b59b6; // Purple
    pub const REVIEW: u32 = 0x5865f2; // Discord blurple
}

/// Manages Discord threads for PR discussions.
pub struct ThreadManager<H: DiscordHttpClient = ReqwestDiscordClient> {
    client: DiscordClient<H>,
    channel_id: String,
    /// Map of PR URL -> Thread state
    threads: Arc<RwLock<HashMap<String, ThreadState>>>,
    /// User ID to mention
    user_id: Option<String>,
}

impl ThreadManager<ReqwestDiscordClient> {
    /// Create a new thread manager.
    pub fn new(
        bot_token: impl Into<String>,
        channel_id: impl Into<String>,
        user_id: Option<String>,
    ) -> Result<Self> {
        let client = DiscordClient::new(bot_token)?;
        Ok(Self {
            client,
            channel_id: channel_id.into(),
            threads: Arc::new(RwLock::new(HashMap::new())),
            user_id,
        })
    }
}

impl<H: DiscordHttpClient> ThreadManager<H> {
    /// Create a new thread manager with a custom Discord client.
    pub fn with_client(
        client: DiscordClient<H>,
        channel_id: impl Into<String>,
        user_id: Option<String>,
    ) -> Self {
        Self {
            client,
            channel_id: channel_id.into(),
            threads: Arc::new(RwLock::new(HashMap::new())),
            user_id,
        }
    }

    /// Get user mention string if configured.
    fn user_mention(&self) -> Option<String> {
        self.user_id.as_ref().map(|id| format!("<@{}>", id))
    }

    /// Create a thread for a new PR.
    pub async fn create_pr_thread(
        &self,
        issue: &Issue,
        pr_url: &str,
        pr_number: i64,
    ) -> Result<ThreadState> {
        // Check if thread already exists
        {
            let threads = self.threads.read().await;
            if let Some(state) = threads.get(pr_url) {
                return Ok(state.clone());
            }
        }

        // Create thread name
        let thread_name = format!("PR #{}: {} ({})", pr_number, issue.short_id, issue.source);

        // Create thread
        let thread = self
            .client
            .create_thread(&self.channel_id, CreateThreadParams::public(&thread_name))
            .await?;

        // Send initial message to thread
        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} New PR created for issue {}", m, issue.short_id),
            None => format!("New PR created for issue {}", issue.short_id),
        };

        let embed = MessageEmbed::new()
            .title(format!("PR Created: {}", issue.short_id))
            .description(&issue.title)
            .url(pr_url)
            .color(colors::SUCCESS)
            .field(
                "Issue",
                format!("[{}]({})", issue.short_id, issue.url),
                true,
            )
            .field("Source", &issue.source, true)
            .field("Priority", issue.priority.to_string(), true)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        let message = self
            .client
            .send_message(&thread.id, CreateMessageParams::with_embed(content, embed))
            .await?;

        // Create and store thread state
        let state = ThreadState {
            thread_id: thread.id.clone(),
            thread_name: thread.name,
            channel_id: self.channel_id.clone(),
            pr_url: pr_url.to_string(),
            issue_id: issue.id.clone(),
            source: issue.source.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            is_active: true,
            last_message_id: Some(message.id),
        };

        {
            let mut threads = self.threads.write().await;
            threads.insert(pr_url.to_string(), state.clone());
        }

        Ok(state)
    }

    /// Post a review notification to the PR thread.
    pub async fn notify_review_submitted(
        &self,
        pr_url: &str,
        reviewer: &str,
        review_state: &str,
        body: Option<&str>,
    ) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let (title, color) = match review_state.to_lowercase().as_str() {
            "approved" => ("Review Approved", colors::SUCCESS),
            "changes_requested" => ("Changes Requested", colors::WARNING),
            "commented" => ("Review Comment", colors::INFO),
            "dismissed" => ("Review Dismissed", colors::PURPLE),
            _ => ("Review Submitted", colors::REVIEW),
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} {} by {}", m, title, reviewer),
            None => format!("{} by {}", title, reviewer),
        };

        let mut embed = MessageEmbed::new()
            .title(title)
            .color(color)
            .field("Reviewer", reviewer, true)
            .field("State", review_state, true)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        if let Some(review_body) = body {
            if !review_body.is_empty() {
                let truncated = if review_body.len() > 1000 {
                    format!(
                        "{}...",
                        &review_body[..review_body.floor_char_boundary(997)]
                    )
                } else {
                    review_body.to_string()
                };
                embed = embed.description(truncated);
            }
        }

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        Ok(())
    }

    /// Post a review comment to the PR thread.
    pub async fn notify_review_comment(
        &self,
        pr_url: &str,
        commenter: &str,
        file_path: Option<&str>,
        comment: &str,
    ) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let truncated_comment = if comment.len() > 1000 {
            format!("{}...", &comment[..comment.floor_char_boundary(997)])
        } else {
            comment.to_string()
        };

        let mut embed = MessageEmbed::new()
            .title("Review Comment")
            .description(&truncated_comment)
            .color(colors::INFO)
            .field("By", commenter, true)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        if let Some(path) = file_path {
            embed = embed.field("File", format!("`{}`", path), true);
        }

        let content = format!("Comment from {}", commenter);

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        Ok(())
    }

    /// Notify that an agent is working on review comments.
    pub async fn notify_agent_started(&self, pr_url: &str, task_description: &str) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} Agent started working on review feedback", m),
            None => "Agent started working on review feedback".to_string(),
        };

        let embed = MessageEmbed::new()
            .title("Agent Working")
            .description(task_description)
            .color(colors::INFO)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        Ok(())
    }

    /// Notify that an agent completed its work.
    pub async fn notify_agent_completed(
        &self,
        pr_url: &str,
        commit_url: Option<&str>,
    ) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} Agent completed review feedback", m),
            None => "Agent completed review feedback".to_string(),
        };

        let mut embed = MessageEmbed::new()
            .title("Agent Completed")
            .description("Review feedback has been addressed")
            .color(colors::SUCCESS)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        if let Some(url) = commit_url {
            embed = embed.field("Commit", format!("[View changes]({})", url), false);
        }

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        Ok(())
    }

    /// Notify that an agent failed.
    pub async fn notify_agent_failed(&self, pr_url: &str, error: &str) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} Agent failed to address review feedback", m),
            None => "Agent failed to address review feedback".to_string(),
        };

        let truncated_error = if error.len() > 1000 {
            format!("{}...", &error[..error.floor_char_boundary(997)])
        } else {
            error.to_string()
        };

        let embed = MessageEmbed::new()
            .title("Agent Failed")
            .description(&truncated_error)
            .color(colors::ERROR)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        Ok(())
    }

    /// Notify that the PR was merged.
    pub async fn notify_pr_merged(&self, pr_url: &str) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} PR merged!", m),
            None => "PR merged!".to_string(),
        };

        let embed = MessageEmbed::new()
            .title("PR Merged")
            .description("The pull request has been merged. Issue resolved.")
            .color(colors::SUCCESS)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        // Archive the thread
        self.client.archive_thread(&thread_id).await?;

        // Update thread state
        {
            let mut threads = self.threads.write().await;
            if let Some(state) = threads.get_mut(pr_url) {
                state.is_active = false;
            }
        }

        Ok(())
    }

    /// Notify that the PR was closed without merging.
    pub async fn notify_pr_closed(&self, pr_url: &str) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => return Ok(()), // No thread for this PR
        };

        let mention = self.user_mention();
        let content = match mention {
            Some(m) => format!("{} PR closed without merging", m),
            None => "PR closed without merging".to_string(),
        };

        let embed = MessageEmbed::new()
            .title("PR Closed")
            .description("The pull request was closed without merging.")
            .color(colors::WARNING)
            .footer("Claudear")
            .timestamp(chrono::Utc::now().to_rfc3339());

        self.client
            .send_message(&thread_id, CreateMessageParams::with_embed(content, embed))
            .await?;

        // Archive the thread
        self.client.archive_thread(&thread_id).await?;

        // Update thread state
        {
            let mut threads = self.threads.write().await;
            if let Some(state) = threads.get_mut(pr_url) {
                state.is_active = false;
            }
        }

        Ok(())
    }

    /// Send a custom message to a PR thread.
    pub async fn send_to_thread(&self, pr_url: &str, message: &str) -> Result<()> {
        let thread_id = {
            let threads = self.threads.read().await;
            threads.get(pr_url).map(|s| s.thread_id.clone())
        };

        let thread_id = match thread_id {
            Some(id) => id,
            None => {
                return Err(Error::notifier(
                    "discord",
                    format!("No thread found for PR: {}", pr_url),
                ))
            }
        };

        self.client
            .send_message(&thread_id, CreateMessageParams::text(message))
            .await?;

        Ok(())
    }

    /// Get thread state for a PR.
    pub async fn get_thread_state(&self, pr_url: &str) -> Option<ThreadState> {
        let threads = self.threads.read().await;
        threads.get(pr_url).cloned()
    }

    /// Load thread states from storage.
    pub async fn load_threads(&self, states: Vec<ThreadState>) {
        let mut threads = self.threads.write().await;
        for state in states {
            if state.is_active {
                threads.insert(state.pr_url.clone(), state);
            }
        }
    }

    /// Get all active thread states.
    pub async fn get_active_threads(&self) -> Vec<ThreadState> {
        let threads = self.threads.read().await;
        threads.values().filter(|s| s.is_active).cloned().collect()
    }

    /// Get all thread states (for persistence).
    pub async fn get_all_threads(&self) -> Vec<ThreadState> {
        let threads = self.threads.read().await;
        threads.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::client::mock::MockDiscordClient;
    use crate::types::{IssuePriority, IssueStatus};

    fn create_test_issue() -> Issue {
        Issue {
            id: "issue-123".to_string(),
            short_id: "TEST-123".to_string(),
            title: "Fix the bug".to_string(),
            description: Some("Bug description".to_string()),
            url: "https://linear.app/test/issue/123".to_string(),
            source: "linear".to_string(),
            priority: IssuePriority::Medium,
            status: IssueStatus::Open,
            metadata: Default::default(),
            created_at: None,
            updated_at: None,
        }
    }

    fn mock_thread_json() -> &'static str {
        r#"{"id": "thread-456", "type": 11, "name": "test-thread", "parent_id": "channel123", "owner_id": "bot123"}"#
    }

    fn mock_message_json() -> &'static str {
        r#"{"id": "msg-789", "channel_id": "thread-456", "content": "Hello", "timestamp": "2024-01-01T00:00:00Z", "author": {"id": "bot123", "username": "bot"}}"#
    }

    fn create_manager_with_mock(mock: MockDiscordClient) -> ThreadManager<MockDiscordClient> {
        let client = DiscordClient::with_http_client("token", mock).unwrap();
        ThreadManager::with_client(client, "channel123", None)
    }

    fn create_manager_with_mock_and_user(
        mock: MockDiscordClient,
    ) -> ThreadManager<MockDiscordClient> {
        let client = DiscordClient::with_http_client("token", mock).unwrap();
        ThreadManager::with_client(client, "channel123", Some("user456".to_string()))
    }

    #[test]
    fn test_thread_manager_requires_token() {
        let result = ThreadManager::new("", "channel123", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_thread_manager_creation() {
        let result = ThreadManager::new("test_token", "channel123", Some("user123".to_string()));
        assert!(result.is_ok());
    }

    #[test]
    fn test_with_client() {
        let mock = MockDiscordClient::new();
        let client = DiscordClient::with_http_client("token", mock).unwrap();
        let manager = ThreadManager::with_client(client, "channel123", Some("user".to_string()));
        assert_eq!(manager.channel_id, "channel123");
        assert_eq!(manager.user_id, Some("user".to_string()));
    }

    #[tokio::test]
    async fn test_thread_state_storage() {
        let manager = ThreadManager::new("test_token", "channel123", None).unwrap();

        let state = ThreadState::new(
            "thread-1",
            "PR: Test",
            "channel123",
            "https://github.com/test/repo/pull/1",
            "issue-1",
            "linear",
        );

        manager.load_threads(vec![state.clone()]).await;

        let retrieved = manager
            .get_thread_state("https://github.com/test/repo/pull/1")
            .await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().thread_id, "thread-1");
    }

    #[tokio::test]
    async fn test_get_active_threads() {
        let manager = ThreadManager::new("test_token", "channel123", None).unwrap();

        let active_state = ThreadState::new(
            "thread-1",
            "PR: Active",
            "channel123",
            "https://github.com/test/repo/pull/1",
            "issue-1",
            "linear",
        );

        let mut inactive_state = ThreadState::new(
            "thread-2",
            "PR: Inactive",
            "channel123",
            "https://github.com/test/repo/pull/2",
            "issue-2",
            "linear",
        );
        inactive_state.is_active = false;

        manager
            .load_threads(vec![active_state, inactive_state])
            .await;

        let active = manager.get_active_threads().await;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].thread_id, "thread-1");
    }

    #[tokio::test]
    async fn test_get_all_threads() {
        let manager = ThreadManager::new("test_token", "channel123", None).unwrap();

        let state1 = ThreadState::new(
            "thread-1",
            "PR: 1",
            "channel123",
            "https://pr/1",
            "issue-1",
            "linear",
        );
        let state2 = ThreadState::new(
            "thread-2",
            "PR: 2",
            "channel123",
            "https://pr/2",
            "issue-2",
            "sentry",
        );
        let mut state3 = ThreadState::new(
            "thread-3",
            "PR: 3",
            "channel123",
            "https://pr/3",
            "issue-3",
            "github",
        );
        state3.is_active = false;

        manager.load_threads(vec![state1, state2, state3]).await;

        // get_all_threads returns all (2 active only since load_threads filters inactive)
        let all = manager.get_all_threads().await;
        assert_eq!(all.len(), 2);
    }

    #[tokio::test]
    async fn test_get_thread_state_not_found() {
        let manager = ThreadManager::new("test_token", "channel123", None).unwrap();

        let result = manager.get_thread_state("https://unknown/pr/999").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_send_to_thread_not_found() {
        let manager = ThreadManager::new("test_token", "channel123", None).unwrap();

        let result = manager
            .send_to_thread("https://unknown/pr/999", "Hello")
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No thread found"));
    }

    #[test]
    fn test_user_mention_with_id() {
        let manager = ThreadManager::new("token", "channel", Some("user123".to_string())).unwrap();
        let mention = manager.user_mention();
        assert_eq!(mention, Some("<@user123>".to_string()));
    }

    #[test]
    fn test_user_mention_without_id() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();
        let mention = manager.user_mention();
        assert!(mention.is_none());
    }

    #[test]
    fn test_colors() {
        assert_eq!(colors::SUCCESS, 0x2ecc71);
        assert_eq!(colors::ERROR, 0xe74c3c);
        assert_eq!(colors::INFO, 0x3498db);
        assert_eq!(colors::WARNING, 0xf39c12);
        assert_eq!(colors::PURPLE, 0x9b59b6);
        assert_eq!(colors::REVIEW, 0x5865f2);
    }

    #[tokio::test]
    async fn test_load_threads_only_loads_active() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let mut inactive = ThreadState::new("t1", "Test", "ch", "https://pr/1", "i1", "linear");
        inactive.is_active = false;

        let active = ThreadState::new("t2", "Test 2", "ch", "https://pr/2", "i2", "sentry");

        manager.load_threads(vec![inactive, active]).await;

        // Only active should be loaded
        let threads = manager.get_all_threads().await;
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].thread_id, "t2");
    }

    #[tokio::test]
    async fn test_notify_review_submitted_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        // Should return Ok when no thread exists
        let result = manager
            .notify_review_submitted("https://unknown", "reviewer", "approved", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_comment_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager
            .notify_review_comment("https://unknown", "commenter", None, "Comment")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_started_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager
            .notify_agent_started("https://unknown", "Working on it")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_completed_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager
            .notify_agent_completed("https://unknown", Some("https://commit"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_failed_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager
            .notify_agent_failed("https://unknown", "Error message")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_pr_merged_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager.notify_pr_merged("https://unknown").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_pr_closed_no_thread() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();

        let result = manager.notify_pr_closed("https://unknown").await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_create_test_issue() {
        let issue = create_test_issue();
        assert_eq!(issue.id, "issue-123");
        assert_eq!(issue.short_id, "TEST-123");
        assert_eq!(issue.source, "linear");
        assert_eq!(issue.priority, IssuePriority::Medium);
    }

    #[tokio::test]
    async fn test_load_empty_threads() {
        let manager = ThreadManager::new("token", "channel", None).unwrap();
        manager.load_threads(vec![]).await;

        let threads = manager.get_all_threads().await;
        assert!(threads.is_empty());
    }

    // Mock-based tests for HTTP-dependent functionality

    #[tokio::test]
    async fn test_create_pr_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/channel123/threads",
            200,
            mock_thread_json(),
        );
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-456/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let issue = create_test_issue();
        let result = manager
            .create_pr_thread(&issue, "https://github.com/test/pr/1", 1)
            .await;

        assert!(result.is_ok());
        let state = result.unwrap();
        assert_eq!(state.thread_id, "thread-456");
        assert_eq!(state.pr_url, "https://github.com/test/pr/1");
        assert_eq!(state.issue_id, "issue-123");
        assert!(state.is_active);
    }

    #[tokio::test]
    async fn test_create_pr_thread_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/channel123/threads",
            200,
            mock_thread_json(),
        );
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-456/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let issue = create_test_issue();
        let result = manager
            .create_pr_thread(&issue, "https://github.com/test/pr/1", 1)
            .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_create_pr_thread_returns_existing() {
        let mock = MockDiscordClient::new();
        let manager = create_manager_with_mock(mock);

        // Pre-load a thread state
        let state = ThreadState::new(
            "existing-thread",
            "PR: Existing",
            "channel123",
            "https://github.com/test/pr/1",
            "issue-123",
            "linear",
        );
        manager.load_threads(vec![state]).await;

        // Calling create_pr_thread for same PR should return existing
        let issue = create_test_issue();
        let result = manager
            .create_pr_thread(&issue, "https://github.com/test/pr/1", 1)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().thread_id, "existing-thread");
    }

    #[tokio::test]
    async fn test_create_pr_thread_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/channel123/threads",
            403,
            "Forbidden",
        );

        let manager = create_manager_with_mock(mock);
        let issue = create_test_issue();
        let result = manager
            .create_pr_thread(&issue, "https://github.com/test/pr/1", 1)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_approved() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "approved", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_changes_requested() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted(
                "https://pr/1",
                "reviewer1",
                "changes_requested",
                Some("Please fix this"),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_commented() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "commented", Some("Looks good"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_dismissed() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "dismissed", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_unknown_state() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "unknown_state", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_with_long_body() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        // Body longer than 1000 chars should be truncated
        let long_body = "x".repeat(1500);
        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "commented", Some(&long_body))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "approved", None)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_comment_with_file() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_comment(
                "https://pr/1",
                "commenter",
                Some("src/main.rs"),
                "Fix this line",
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_comment_without_file() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_comment("https://pr/1", "commenter", None, "General comment")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_review_comment_long_comment() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let long_comment = "x".repeat(1500);
        let result = manager
            .notify_review_comment("https://pr/1", "commenter", None, &long_comment)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_started_with_thread() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_agent_started("https://pr/1", "Working on fixes")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_started_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_agent_started("https://pr/1", "Working on fixes")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_completed_with_commit() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_agent_completed("https://pr/1", Some("https://github.com/commit/abc123"))
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_completed_without_commit() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_agent_completed("https://pr/1", None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_completed_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_agent_completed("https://pr/1", None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_failed_with_thread() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_agent_failed("https://pr/1", "Error occurred")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_failed_long_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let long_error = "x".repeat(1500);
        let result = manager
            .notify_agent_failed("https://pr/1", &long_error)
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_agent_failed_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_agent_failed("https://pr/1", "Error").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_pr_merged_with_thread() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            200,
            mock_thread_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_merged("https://pr/1").await;
        assert!(result.is_ok());

        // Thread should be marked inactive
        let thread_state = manager.get_thread_state("https://pr/1").await.unwrap();
        assert!(!thread_state.is_active);
    }

    #[tokio::test]
    async fn test_notify_pr_merged_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            200,
            mock_thread_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_merged("https://pr/1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_pr_closed_with_thread() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            200,
            mock_thread_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_closed("https://pr/1").await;
        assert!(result.is_ok());

        // Thread should be marked inactive
        let thread_state = manager.get_thread_state("https://pr/1").await.unwrap();
        assert!(!thread_state.is_active);
    }

    #[tokio::test]
    async fn test_notify_pr_closed_with_user_mention() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            200,
            mock_thread_json(),
        );

        let manager = create_manager_with_mock_and_user(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_closed("https://pr/1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_to_thread_success() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .send_to_thread("https://pr/1", "Hello thread!")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_to_thread_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            403,
            "Forbidden",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.send_to_thread("https://pr/1", "Hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "approved", None)
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_review_comment_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_review_comment("https://pr/1", "commenter", None, "Comment")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_agent_started_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager
            .notify_agent_started("https://pr/1", "Working")
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_agent_completed_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_agent_completed("https://pr/1", None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_agent_failed_api_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_agent_failed("https://pr/1", "Error").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_pr_merged_send_message_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_merged("https://pr/1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_pr_merged_archive_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            500,
            "Archive failed",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_merged("https://pr/1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_pr_closed_send_message_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            500,
            "Internal error",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_closed("https://pr/1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_pr_closed_archive_error() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );
        mock.mock_patch(
            "https://discord.com/api/v10/channels/thread-1",
            500,
            "Archive failed",
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        let result = manager.notify_pr_closed("https://pr/1").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_notify_review_submitted_empty_body() {
        let mock = MockDiscordClient::new();
        mock.mock_post(
            "https://discord.com/api/v10/channels/thread-1/messages",
            200,
            mock_message_json(),
        );

        let manager = create_manager_with_mock(mock);
        let state = ThreadState::new("thread-1", "PR: Test", "ch", "https://pr/1", "i1", "linear");
        manager.load_threads(vec![state]).await;

        // Empty body should not add description to embed
        let result = manager
            .notify_review_submitted("https://pr/1", "reviewer1", "commented", Some(""))
            .await;
        assert!(result.is_ok());
    }
}

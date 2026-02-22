//! Telegram notifier via Telegram Bot API.

use super::Notifier;
use crate::config::TelegramConfig;
use crate::error::{Error, Result};
use crate::http::HttpResponse;
use crate::types::{AskDelivery, AskRequest, Issue};
use crate::users::UserRegistry;
use async_trait::async_trait;

/// Trait for HTTP client used by Telegram notifier.
#[async_trait]
pub trait TelegramHttpClient: Send + Sync {
    async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse>;
}

/// Real HTTP client using reqwest.
pub struct ReqwestTelegramClient {
    client: reqwest::Client,
}

impl ReqwestTelegramClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
        }
    }
}

impl Default for ReqwestTelegramClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TelegramHttpClient for ReqwestTelegramClient {
    async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse> {
        let response = self.client.post(url).json(body).send().await?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        Ok(HttpResponse { status, body })
    }
}

/// Telegram notifier that sends notifications via Telegram Bot API.
pub struct TelegramNotifier<H: TelegramHttpClient = ReqwestTelegramClient> {
    config: TelegramConfig,
    http: H,
    user_registry: UserRegistry,
}

impl TelegramNotifier<ReqwestTelegramClient> {
    /// Create a new Telegram notifier.
    pub fn new(config: TelegramConfig, user_registry: UserRegistry) -> Self {
        Self {
            config,
            http: ReqwestTelegramClient::new(),
            user_registry,
        }
    }
}

impl<H: TelegramHttpClient> TelegramNotifier<H> {
    /// Create a new Telegram notifier with custom HTTP client.
    pub fn with_http_client(config: TelegramConfig, http: H) -> Self {
        Self {
            config,
            http,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
        }
    }

    /// Create a new Telegram notifier with custom HTTP client and user registry.
    pub fn with_http_client_and_registry(
        config: TelegramConfig,
        http: H,
        user_registry: UserRegistry,
    ) -> Self {
        Self {
            config,
            http,
            user_registry,
        }
    }

    fn resolve_recipients(&self, issue: Option<&Issue>) -> Vec<String> {
        if let Some(issue) = issue {
            if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
                if let Some(user) = self.user_registry.get_by_slug(&slug) {
                    if let Some(ref chat_id) = user.telegram_chat_id {
                        return vec![chat_id.clone()];
                    }
                }
            }
        }

        // Fall back: collect chat_id + to_chat_ids from config
        let mut recipients = Vec::new();
        if let Some(ref chat_id) = self.config.chat_id {
            recipients.push(chat_id.clone());
        }
        for chat_id in &self.config.to_chat_ids {
            recipients.push(chat_id.clone());
        }
        recipients
    }

    async fn send_message(&self, text: &str, issue: Option<&Issue>) -> Result<()> {
        let bot_token = match &self.config.bot_token {
            Some(token) => token.expose(),
            None => return Ok(()),
        };

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            bot_token
        );

        // Truncate message to Telegram limit (4096 chars)
        let truncated_text = if text.len() > 4096 {
            format!("{}...", &text[..text.floor_char_boundary(4093)])
        } else {
            text.to_string()
        };

        let recipients = self.resolve_recipients(issue);

        for chat_id in &recipients {
            let body = serde_json::json!({
                "chat_id": chat_id,
                "text": truncated_text,
                "parse_mode": "HTML"
            });

            let response = self.http.post_json(&url, &body).await?;

            if response.status < 200 || response.status >= 300 {
                return Err(Error::notifier(
                    "telegram",
                    format!("Telegram API error: {}", response.body),
                ));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl<H: TelegramHttpClient + 'static> Notifier for TelegramNotifier<H> {
    fn name(&self) -> &str {
        "telegram"
    }

    fn is_enabled(&self) -> bool {
        self.config.bot_token.is_some()
            && (self.config.chat_id.is_some() || !self.config.to_chat_ids.is_empty())
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let body = format!(
            "[Claudear] Processing {} from {} - {}",
            issue.short_id, issue.source, issue.title
        );
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let body = if issue
            .get_metadata::<String>("cascade_downstream_repo")
            .is_some()
        {
            let downstream = issue
                .get_metadata::<String>("cascade_downstream_repo")
                .unwrap_or_default();
            format!(
                "[Claudear] Cascade PR for {} ({}): {}",
                issue.short_id, downstream, pr_url
            )
        } else if issue.get_metadata::<bool>("is_pr_update").unwrap_or(false) {
            format!("[Claudear] PR Updated for {}: {}", issue.short_id, pr_url)
        } else {
            format!("[Claudear] PR Created for {}: {}", issue.short_id, pr_url)
        };
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let body = if issue
            .get_metadata::<bool>("regression_resolved")
            .unwrap_or(false)
        {
            format!(
                "[Claudear] Regression Resolved: {} (no regression after monitoring)",
                issue.short_id
            )
        } else {
            format!("[Claudear] Completed {} (no PR URL)", issue.short_id)
        };
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let short_error = if error.len() > 100 {
            format!("{}...", &error[..error.floor_char_boundary(97)])
        } else {
            error.to_string()
        };

        let body = if issue
            .get_metadata::<bool>("regression_detected")
            .unwrap_or(false)
        {
            format!("[Claudear] REGRESSION {}: {}", issue.short_id, short_error)
        } else if issue
            .get_metadata::<String>("cascade_downstream_repo")
            .is_some()
        {
            let downstream = issue
                .get_metadata::<String>("cascade_downstream_repo")
                .unwrap_or_default();
            format!(
                "[Claudear] CASCADE FAILED {} ({}): {}",
                issue.short_id, downstream, short_error
            )
        } else {
            format!("[Claudear] FAILED {}: {}", issue.short_id, short_error)
        };
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_merged(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let body = format!("[Claudear] PR Merged for {}: {}", issue.short_id, pr_url);
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_closed(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let body = format!("[Claudear] PR Closed for {}: {}", issue.short_id, pr_url);
        self.send_message(&body, Some(issue)).await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let body = format!("[Claudear] {}", message);
        self.send_message(&body, None).await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let body = format!(
            "[Claudear] {} urgent issue(s): {}",
            issues.len(),
            issues
                .iter()
                .take(3)
                .map(|i| i.short_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.send_message(&body, None).await
    }

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let body = format!(
            "[Claudear] Human input needed for {}: {}",
            issue.short_id, request.question.question
        );
        self.send_message(&body, Some(issue)).await?;
        Ok(Some(AskDelivery {
            channel: "telegram".to_string(),
            target: None,
            message_id: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn empty_registry() -> UserRegistry {
        UserRegistry::new(std::collections::HashMap::new())
    }

    /// Mock Telegram HTTP client for testing.
    struct MockTelegramClient {
        response_status: u16,
        response_body: String,
        call_count: AtomicUsize,
        last_calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockTelegramClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response_status: status,
                response_body: body.to_string(),
                call_count: AtomicUsize::new(0),
                last_calls: Mutex::new(Vec::new()),
            }
        }

        fn success() -> Self {
            Self::new(200, r#"{"ok": true, "result": {"message_id": 42}}"#)
        }

        fn error(status: u16, body: &str) -> Self {
            Self::new(status, body)
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn get_last_calls(&self) -> Vec<(String, serde_json::Value)> {
            self.last_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TelegramHttpClient for MockTelegramClient {
        async fn post_json(&self, url: &str, body: &serde_json::Value) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.last_calls
                .lock()
                .unwrap()
                .push((url.to_string(), body.clone()));

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }
    }

    fn disabled_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: None,
            chat_id: None,
            to_chat_ids: vec![],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn enabled_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: Some("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11".into()),
            chat_id: Some("987654321".to_string()),
            to_chat_ids: vec![],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn multi_recipient_config() -> TelegramConfig {
        TelegramConfig {
            bot_token: Some("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11".into()),
            chat_id: Some("111111111".to_string()),
            to_chat_ids: vec!["222222222".to_string(), "333333333".to_string()],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn partial_config_no_token() -> TelegramConfig {
        TelegramConfig {
            bot_token: None,
            chat_id: Some("987654321".to_string()),
            to_chat_ids: vec![],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn partial_config_no_chat_id() -> TelegramConfig {
        TelegramConfig {
            bot_token: Some("token".into()),
            chat_id: None,
            to_chat_ids: vec![],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    fn config_with_to_chat_ids_only() -> TelegramConfig {
        TelegramConfig {
            bot_token: Some("token".into()),
            chat_id: None,
            to_chat_ids: vec!["444444444".to_string()],
            source_enabled: false,
            listen_chat_id: None,
            poll_interval_ms: None,
        }
    }

    // --- Basic trait tests ---

    #[test]
    fn test_name() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        assert_eq!(notifier.name(), "telegram");
    }

    #[test]
    fn test_is_enabled() {
        let notifier = TelegramNotifier::new(enabled_config(), empty_registry());
        assert!(notifier.is_enabled());

        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_partial_configs() {
        assert!(!TelegramNotifier::new(partial_config_no_token(), empty_registry()).is_enabled());
        assert!(!TelegramNotifier::new(partial_config_no_chat_id(), empty_registry()).is_enabled());
    }

    #[test]
    fn test_is_enabled_with_to_chat_ids_only() {
        assert!(
            TelegramNotifier::new(config_with_to_chat_ids_only(), empty_registry()).is_enabled()
        );
    }

    // --- Disabled config tests (no HTTP calls) ---

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_failed(&issue, "Error message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_long_error() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let long_error = "x".repeat(200);
        let result = notifier.notify_failed(&issue, &long_error).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_status("Status update").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_disabled() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncated_to_three() {
        let notifier = TelegramNotifier::new(disabled_config(), empty_registry());
        let issues: Vec<Issue> = (0..10)
            .map(|i| {
                Issue::new(
                    format!("{}", i),
                    format!("PROJ-{}", i),
                    format!("Issue {}", i),
                    "https://example.com",
                    "linear",
                )
            })
            .collect();

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_new_multiple_recipients() {
        let notifier = TelegramNotifier::new(multi_recipient_config(), empty_registry());
        assert!(notifier.is_enabled());
    }

    // --- Mock-based tests for HTTP-dependent functionality ---

    #[tokio::test]
    async fn test_send_message_success() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_send_message_verifies_url_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].0.contains("api.telegram.org"));
        assert!(calls[0].0.contains("/bot"));
        assert!(calls[0].0.contains("/sendMessage"));
        // Token should be in the URL
        assert!(calls[0]
            .0
            .contains("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"));
    }

    #[tokio::test]
    async fn test_send_message_no_auth_header() {
        // Telegram uses token-in-URL, not an auth header.
        // The mock only receives url + body (no auth params), verifying no auth header is used.
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls.len(), 1);
        // The body should contain chat_id, text, and parse_mode
        let body = &calls[0].1;
        assert!(body.get("chat_id").is_some());
        assert!(body.get("text").is_some());
        assert_eq!(body["parse_mode"], "HTML");
    }

    #[tokio::test]
    async fn test_send_message_sends_correct_body() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = &calls[0].1;
        assert_eq!(body["chat_id"], "987654321");
        assert!(body["text"].as_str().unwrap().contains("Processing"));
        assert_eq!(body["parse_mode"], "HTML");
    }

    #[tokio::test]
    async fn test_send_message_multiple_recipients() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(multi_recipient_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_ok());
        // chat_id + 2 to_chat_ids = 3 calls
        assert_eq!(notifier.http.get_call_count(), 3);
    }

    #[tokio::test]
    async fn test_send_message_error_response() {
        let mock = MockTelegramClient::error(400, r#"{"ok":false,"description":"Bad Request"}"#);
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("Telegram API error"));
        assert!(err_str.contains("Bad Request"));
    }

    #[tokio::test]
    async fn test_send_message_server_error() {
        let mock = MockTelegramClient::error(500, "Internal server error");
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_send_message_truncates_long_message() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);

        // Create a message longer than 4096 chars
        let long_message = "x".repeat(5000);
        notifier.notify_status(&long_message).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        // Body should be truncated to 4096 chars + "..."
        assert!(text.len() <= 4200); // "[Claudear] " + truncated body
        assert!(text.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_success_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/42")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("[Claudear]"));
        assert!(text.contains("PR Created"));
        assert!(text.contains("PROJ-123"));
        assert!(text.contains("https://github.com/org/repo/pull/42"));
    }

    #[tokio::test]
    async fn test_notify_completed_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_completed(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("Completed"));
        assert!(text.contains("no PR URL"));
    }

    #[tokio::test]
    async fn test_notify_failed_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier
            .notify_failed(&issue, "Build failed with exit code 1")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("FAILED"));
        assert!(text.contains("PROJ-123"));
        assert!(text.contains("Build failed"));
    }

    #[tokio::test]
    async fn test_notify_failed_truncates_long_error() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let long_error = "x".repeat(200);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        // Error should be truncated to 100 chars including "..."
        assert!(text.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_status_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("System is healthy").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert_eq!(text, "[Claudear] System is healthy");
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("2 urgent issue(s)"));
        assert!(text.contains("PROJ-1"));
        assert!(text.contains("PROJ-2"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncates_to_three() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issues: Vec<Issue> = (1..=10)
            .map(|i| {
                Issue::new(
                    i.to_string(),
                    format!("PROJ-{}", i),
                    format!("Issue {}", i),
                    "https://example.com",
                    "linear",
                )
            })
            .collect();

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("10 urgent issue(s)"));
        // Only first 3 are listed
        assert!(text.contains("PROJ-1"));
        assert!(text.contains("PROJ-2"));
        assert!(text.contains("PROJ-3"));
        assert!(!text.contains("PROJ-4"));
    }

    #[tokio::test]
    async fn test_send_message_stops_on_first_error() {
        let mock = MockTelegramClient::error(400, "Bad request");
        let notifier = TelegramNotifier::with_http_client(multi_recipient_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_err());
        // Should stop after first failure, not try all 3 recipients
        assert_eq!(notifier.http.get_call_count(), 1);
    }

    #[test]
    fn test_with_http_client() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);

        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "telegram");
    }

    #[test]
    fn test_reqwest_telegram_client_default() {
        let client = ReqwestTelegramClient::default();
        // Just verify it can be constructed
        assert!(std::mem::size_of_val(&client) > 0);
    }

    #[test]
    fn test_resolve_recipients_returns_config_chat_ids_when_no_issue() {
        let notifier =
            TelegramNotifier::with_http_client(multi_recipient_config(), MockTelegramClient::success());
        let recipients = notifier.resolve_recipients(None);
        assert_eq!(
            recipients,
            vec![
                "111111111".to_string(),
                "222222222".to_string(),
                "333333333".to_string()
            ]
        );
    }

    #[test]
    fn test_resolve_recipients_returns_config_chat_ids_when_no_resolved_user() {
        let notifier =
            TelegramNotifier::with_http_client(enabled_config(), MockTelegramClient::success());
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["987654321".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_uses_resolved_user_telegram_chat_id() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                telegram_chat_id: Some("555555555".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier = TelegramNotifier::with_http_client_and_registry(
            enabled_config(),
            MockTelegramClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["555555555".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_falls_back_when_user_has_no_telegram() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                telegram_chat_id: None,
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier = TelegramNotifier::with_http_client_and_registry(
            enabled_config(),
            MockTelegramClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        // Falls back to config chat_id
        assert_eq!(recipients, vec!["987654321".to_string()]);
    }

    #[tokio::test]
    async fn test_ask_question_message_contains_question() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test Issue", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-tg-1".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        notifier.ask_question(&issue, &request).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(!text.contains("[CLAUDEAR-Q:"));
        assert!(text.contains("Human input needed for LIN-1"));
        assert!(text.contains("Which branch?"));
    }

    #[tokio::test]
    async fn test_ask_question_delivery_channel_is_telegram() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-tg-2".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Q?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: chrono::Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.channel, "telegram");
        assert!(delivery.target.is_none());
        assert!(delivery.message_id.is_none());
    }

    #[tokio::test]
    async fn test_notify_start_message_includes_source_and_title() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "SEN-42",
            "Memory leak in worker",
            "https://sentry.io/42",
            "sentry",
        );
        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("SEN-42"));
        assert!(text.contains("sentry"));
        assert!(text.contains("Memory leak in worker"));
    }

    #[tokio::test]
    async fn test_notify_failed_short_error_not_truncated() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        notifier.notify_failed(&issue, "Short error").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("Short error"));
        assert!(!text.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_failed_exact_100_char_error_not_truncated() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        let error = "x".repeat(100);
        notifier.notify_failed(&issue, &error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains(&error));
        assert!(!text.ends_with("..."));
    }

    #[tokio::test]
    async fn test_send_message_within_limit_not_truncated() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);

        let message = "x".repeat(100);
        notifier.notify_status(&message).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(!text.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_routes_to_resolved_user_telegram_chat_id() {
        let mock = MockTelegramClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                telegram_chat_id: Some("999999999".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier =
            TelegramNotifier::with_http_client_and_registry(enabled_config(), mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls[0].1["chat_id"], "999999999");
    }

    // --- Tests for cascade success message ---

    #[tokio::test]
    async fn test_notify_success_cascade_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        notifier
            .notify_success(&issue, "https://github.com/downstream/repo/pull/5")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("Cascade PR"));
        assert!(text.contains("LIN-1"));
        assert!(text.contains("downstream/repo"));
        assert!(text.contains("https://github.com/downstream/repo/pull/5"));
    }

    // --- Tests for PR update success message ---

    #[tokio::test]
    async fn test_notify_success_pr_update_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("is_pr_update", true);

        notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/77")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("PR Updated"));
        assert!(text.contains("LIN-1"));
        assert!(text.contains("https://github.com/org/repo/pull/77"));
    }

    // --- Tests for regression resolved completed message ---

    #[tokio::test]
    async fn test_notify_completed_regression_resolved_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_resolved", true);

        notifier.notify_completed(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("Regression Resolved"));
        assert!(text.contains("SEN-1"));
        assert!(text.contains("no regression"));
    }

    // --- Tests for regression detected failed message ---

    #[tokio::test]
    async fn test_notify_failed_regression_detected_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_detected", true);

        notifier
            .notify_failed(&issue, "Tests failing again")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("REGRESSION"));
        assert!(text.contains("SEN-1"));
        assert!(text.contains("Tests failing again"));
    }

    // --- Tests for cascade failed message ---

    #[tokio::test]
    async fn test_notify_failed_cascade_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        notifier.notify_failed(&issue, "Build error").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("CASCADE FAILED"));
        assert!(text.contains("LIN-1"));
        assert!(text.contains("downstream/repo"));
        assert!(text.contains("Build error"));
    }

    // --- Tests for notify_merged and notify_closed ---

    #[tokio::test]
    async fn test_notify_merged_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Fix", "https://example.com", "linear");

        notifier
            .notify_merged(&issue, "https://github.com/org/repo/pull/42")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("PR Merged"));
        assert!(text.contains("PROJ-1"));
        assert!(text.contains("https://github.com/org/repo/pull/42"));
    }

    #[tokio::test]
    async fn test_notify_closed_message_format() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Fix", "https://example.com", "linear");

        notifier
            .notify_closed(&issue, "https://github.com/org/repo/pull/43")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("PR Closed"));
        assert!(text.contains("PROJ-1"));
        assert!(text.contains("https://github.com/org/repo/pull/43"));
    }

    // --- Test failed cascade with long error truncation ---

    #[tokio::test]
    async fn test_notify_failed_cascade_truncates_long_error() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        let long_error = "e".repeat(200);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("CASCADE FAILED"));
        assert!(text.contains("..."));
    }

    // --- Test regression with long error truncation ---

    #[tokio::test]
    async fn test_notify_failed_regression_truncates_long_error() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_detected", true);

        let long_error = "r".repeat(200);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let text = calls[0].1["text"].as_str().unwrap();
        assert!(text.contains("REGRESSION"));
        assert!(text.contains("..."));
    }

    // --- Test parse_mode is always HTML ---

    #[tokio::test]
    async fn test_all_messages_use_html_parse_mode() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        notifier.notify_start(&issue).await.unwrap();
        notifier
            .notify_success(&issue, "https://example.com/pr")
            .await
            .unwrap();
        notifier.notify_completed(&issue).await.unwrap();
        notifier.notify_failed(&issue, "error").await.unwrap();
        notifier.notify_status("status").await.unwrap();

        let calls = notifier.http.get_last_calls();
        for (i, call) in calls.iter().enumerate() {
            assert_eq!(
                call.1["parse_mode"], "HTML",
                "Call {} should use HTML parse mode",
                i
            );
        }
    }

    // --- Test multiple recipients get the same text ---

    #[tokio::test]
    async fn test_multiple_recipients_receive_same_text() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(multi_recipient_config(), mock);

        notifier.notify_status("Broadcast message").await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls.len(), 3);
        let expected_text = "[Claudear] Broadcast message";
        for call in &calls {
            assert_eq!(call.1["text"].as_str().unwrap(), expected_text);
        }
        // Verify different chat_ids
        assert_eq!(calls[0].1["chat_id"], "111111111");
        assert_eq!(calls[1].1["chat_id"], "222222222");
        assert_eq!(calls[2].1["chat_id"], "333333333");
    }

    // --- Test config with only to_chat_ids (no primary chat_id) ---

    #[tokio::test]
    async fn test_config_with_only_to_chat_ids() {
        let mock = MockTelegramClient::success();
        let notifier = TelegramNotifier::with_http_client(config_with_to_chat_ids_only(), mock);

        notifier.notify_status("test").await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1["chat_id"], "444444444");
    }

    // --- Test http_response_fields ---

    #[test]
    fn test_http_response_fields() {
        let response = HttpResponse {
            status: 201,
            body: "Created".to_string(),
        };
        assert_eq!(response.status, 201);
        assert_eq!(response.body, "Created");
    }
}

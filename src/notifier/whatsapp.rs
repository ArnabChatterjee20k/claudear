//! WhatsApp notifier via WhatsApp Business Cloud API.

use super::Notifier;
use crate::config::WhatsAppConfig;
use crate::error::{Error, Result};
use crate::http::HttpResponse;
use crate::types::{AskDelivery, AskRequest, Issue};
use crate::users::UserRegistry;
use async_trait::async_trait;

/// Trait for HTTP client used by WhatsApp notifier.
#[async_trait]
pub trait WhatsAppHttpClient: Send + Sync {
    async fn post_json(
        &self,
        url: &str,
        bearer_token: &str,
        body: &serde_json::Value,
    ) -> Result<HttpResponse>;
}

/// Real HTTP client using reqwest.
pub struct ReqwestWhatsAppClient {
    client: reqwest::Client,
}

impl ReqwestWhatsAppClient {
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

impl Default for ReqwestWhatsAppClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WhatsAppHttpClient for ReqwestWhatsAppClient {
    async fn post_json(
        &self,
        url: &str,
        bearer_token: &str,
        body: &serde_json::Value,
    ) -> Result<HttpResponse> {
        let response = self
            .client
            .post(url)
            .bearer_auth(bearer_token)
            .json(body)
            .send()
            .await?;

        let status = response.status().as_u16();
        let resp_body = response.text().await.unwrap_or_default();

        Ok(HttpResponse {
            status,
            body: resp_body,
        })
    }
}

/// WhatsApp notifier that sends notifications via WhatsApp Business Cloud API.
pub struct WhatsAppNotifier<H: WhatsAppHttpClient = ReqwestWhatsAppClient> {
    config: WhatsAppConfig,
    http: H,
    user_registry: UserRegistry,
}

impl WhatsAppNotifier<ReqwestWhatsAppClient> {
    /// Create a new WhatsApp notifier.
    pub fn new(config: WhatsAppConfig, user_registry: UserRegistry) -> Self {
        Self {
            config,
            http: ReqwestWhatsAppClient::new(),
            user_registry,
        }
    }
}

impl<H: WhatsAppHttpClient> WhatsAppNotifier<H> {
    /// Create a new WhatsApp notifier with custom HTTP client.
    pub fn with_http_client(config: WhatsAppConfig, http: H) -> Self {
        Self {
            config,
            http,
            user_registry: UserRegistry::new(std::collections::HashMap::new()),
        }
    }

    /// Create a new WhatsApp notifier with custom HTTP client and user registry.
    pub fn with_http_client_and_registry(
        config: WhatsAppConfig,
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
                    if let Some(ref number) = user.whatsapp_number {
                        return vec![number.clone()];
                    }
                }
            }
        }
        self.config.to_numbers.clone()
    }

    async fn send_message(&self, body: &str, issue: Option<&Issue>) -> Result<()> {
        let (phone_number_id, access_token) =
            match (&self.config.phone_number_id, &self.config.access_token) {
                (Some(pid), Some(token)) => (pid, token.expose()),
                _ => return Ok(()),
            };

        let url = format!(
            "https://graph.facebook.com/v21.0/{}/messages",
            phone_number_id
        );

        // Truncate message to WhatsApp limit (4096 chars)
        let truncated_body = if body.len() > 4096 {
            format!("{}...", &body[..body.floor_char_boundary(4093)])
        } else {
            body.to_string()
        };

        let recipients = self.resolve_recipients(issue);

        for to_number in &recipients {
            let payload = serde_json::json!({
                "messaging_product": "whatsapp",
                "to": to_number,
                "type": "text",
                "text": {
                    "body": truncated_body
                }
            });

            let response = self.http.post_json(&url, access_token, &payload).await?;

            if response.status < 200 || response.status >= 300 {
                return Err(Error::notifier(
                    "whatsapp",
                    format!("WhatsApp API error: {}", response.body),
                ));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl<H: WhatsAppHttpClient + 'static> Notifier for WhatsAppNotifier<H> {
    fn name(&self) -> &str {
        "whatsapp"
    }

    fn is_enabled(&self) -> bool {
        self.config.phone_number_id.is_some()
            && self.config.access_token.is_some()
            && !self.config.to_numbers.is_empty()
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
            channel: "whatsapp".to_string(),
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

    /// Mock WhatsApp HTTP client for testing.
    struct MockWhatsAppClient {
        response_status: u16,
        response_body: String,
        call_count: AtomicUsize,
        last_calls: Mutex<Vec<(String, String, serde_json::Value)>>,
    }

    impl MockWhatsAppClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response_status: status,
                response_body: body.to_string(),
                call_count: AtomicUsize::new(0),
                last_calls: Mutex::new(Vec::new()),
            }
        }

        fn success() -> Self {
            Self::new(
                200,
                r#"{"messaging_product":"whatsapp","contacts":[{"wa_id":"15559876543"}],"messages":[{"id":"wamid.xxx"}]}"#,
            )
        }

        fn error(status: u16, body: &str) -> Self {
            Self::new(status, body)
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        fn get_last_calls(&self) -> Vec<(String, String, serde_json::Value)> {
            self.last_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl WhatsAppHttpClient for MockWhatsAppClient {
        async fn post_json(
            &self,
            url: &str,
            bearer_token: &str,
            body: &serde_json::Value,
        ) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.last_calls.lock().unwrap().push((
                url.to_string(),
                bearer_token.to_string(),
                body.clone(),
            ));

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }
    }

    fn disabled_config() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: None,
            access_token: None,
            to_numbers: vec![],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    fn enabled_config() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: Some("123456789".to_string()),
            access_token: Some("access_token_xyz".into()),
            to_numbers: vec!["+15559876543".to_string()],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    fn multi_recipient_config() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: Some("123456789".to_string()),
            access_token: Some("access_token_xyz".into()),
            to_numbers: vec![
                "+15551111111".to_string(),
                "+15552222222".to_string(),
                "+15553333333".to_string(),
            ],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    fn partial_config_no_phone_id() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: None,
            access_token: Some("token".into()),
            to_numbers: vec!["+0987654321".to_string()],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    fn partial_config_no_token() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: Some("pid".to_string()),
            access_token: None,
            to_numbers: vec!["+0987654321".to_string()],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    fn partial_config_no_to() -> WhatsAppConfig {
        WhatsAppConfig {
            phone_number_id: Some("pid".to_string()),
            access_token: Some("token".into()),
            to_numbers: vec![],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        }
    }

    // --- Basic trait tests ---

    #[test]
    fn test_name() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        assert_eq!(notifier.name(), "whatsapp");
    }

    #[test]
    fn test_is_enabled() {
        let notifier = WhatsAppNotifier::new(enabled_config(), empty_registry());
        assert!(notifier.is_enabled());

        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_partial_configs() {
        assert!(
            !WhatsAppNotifier::new(partial_config_no_phone_id(), empty_registry()).is_enabled()
        );
        assert!(!WhatsAppNotifier::new(partial_config_no_token(), empty_registry()).is_enabled());
        assert!(!WhatsAppNotifier::new(partial_config_no_to(), empty_registry()).is_enabled());
    }

    // --- Disabled config tests (silent no-op) ---

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_failed(&issue, "Error message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_long_error() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let long_error = "x".repeat(200);
        let result = notifier.notify_failed(&issue, &long_error).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_status("Status update").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());

        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_disabled() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncated_to_three() {
        let notifier = WhatsAppNotifier::new(disabled_config(), empty_registry());
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
        let notifier = WhatsAppNotifier::new(multi_recipient_config(), empty_registry());
        assert!(notifier.is_enabled());
    }

    // --- Mock-based tests for HTTP-dependent functionality ---

    #[tokio::test]
    async fn test_send_message_success() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        assert!(calls[0].0.contains("graph.facebook.com"));
        assert!(calls[0].0.contains("v21.0"));
        assert!(calls[0].0.contains("123456789")); // phone_number_id in URL
        assert!(calls[0].0.contains("messages"));
    }

    #[tokio::test]
    async fn test_send_message_uses_bearer_auth() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls[0].1, "access_token_xyz"); // bearer_token
    }

    #[tokio::test]
    async fn test_send_message_sends_correct_json_body() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = &calls[0].2;
        assert_eq!(body["messaging_product"], "whatsapp");
        assert_eq!(body["to"], "+15559876543");
        assert_eq!(body["type"], "text");
        assert!(body["text"]["body"]
            .as_str()
            .unwrap()
            .contains("Processing"));
    }

    #[tokio::test]
    async fn test_send_message_multiple_recipients() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(multi_recipient_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        let result = notifier.notify_start(&issue).await;

        assert!(result.is_ok());
        assert_eq!(notifier.http.get_call_count(), 3); // One call per recipient
    }

    #[tokio::test]
    async fn test_send_message_error_response() {
        let mock = MockWhatsAppClient::error(400, "Invalid phone number");
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        assert!(err_str.contains("WhatsApp API error"));
        assert!(err_str.contains("Invalid phone number"));
    }

    #[tokio::test]
    async fn test_send_message_server_error() {
        let mock = MockWhatsAppClient::error(500, "Internal server error");
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);

        // Create a message longer than 4096 chars
        let long_message = "x".repeat(5000);
        notifier.notify_status(&long_message).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body_text = calls[0].2["text"]["body"].as_str().unwrap();
        // Body should be truncated to 4096 chars + "..."
        assert!(body_text.len() <= 4200); // "[Claudear] " + truncated body
        assert!(body_text.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_success_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("[Claudear]"));
        assert!(body.contains("PR Created"));
        assert!(body.contains("PROJ-123"));
        assert!(body.contains("https://github.com/org/repo/pull/42"));
    }

    #[tokio::test]
    async fn test_notify_completed_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_completed(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("Completed"));
        assert!(body.contains("no PR URL"));
    }

    #[tokio::test]
    async fn test_notify_failed_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("FAILED"));
        assert!(body.contains("PROJ-123"));
        assert!(body.contains("Build failed"));
    }

    #[tokio::test]
    async fn test_notify_failed_truncates_long_error() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        // Error should be truncated to 100 chars including "..."
        assert!(body.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_status_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("System is healthy").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert_eq!(body, "[Claudear] System is healthy");
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("2 urgent issue(s)"));
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("PROJ-2"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncates_to_three() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
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
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("10 urgent issue(s)"));
        // Only first 3 are listed
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("PROJ-2"));
        assert!(body.contains("PROJ-3"));
        assert!(!body.contains("PROJ-4"));
    }

    #[tokio::test]
    async fn test_send_message_stops_on_first_error() {
        let mock = MockWhatsAppClient::error(400, "Bad request");
        let notifier = WhatsAppNotifier::with_http_client(multi_recipient_config(), mock);
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
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);

        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "whatsapp");
    }

    #[test]
    fn test_reqwest_whatsapp_client_default() {
        let client = ReqwestWhatsAppClient::default();
        // Just verify it can be constructed
        assert!(std::mem::size_of_val(&client) > 0);
    }

    #[test]
    fn test_resolve_recipients_returns_config_numbers_when_no_issue() {
        let config = WhatsAppConfig {
            phone_number_id: Some("pid".to_string()),
            access_token: Some("token".into()),
            to_numbers: vec!["+1111".to_string(), "+2222".to_string()],
            source_enabled: false,
            listen_phone_number_id: None,
            poll_interval_ms: None,
        };
        let notifier = WhatsAppNotifier::with_http_client(config, MockWhatsAppClient::success());
        let recipients = notifier.resolve_recipients(None);
        assert_eq!(recipients, vec!["+1111".to_string(), "+2222".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_returns_config_numbers_when_no_resolved_user() {
        let notifier =
            WhatsAppNotifier::with_http_client(enabled_config(), MockWhatsAppClient::success());
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["+15559876543".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_uses_resolved_user_whatsapp_number() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                whatsapp_number: Some("+15550001111".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier = WhatsAppNotifier::with_http_client_and_registry(
            enabled_config(),
            MockWhatsAppClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["+15550001111".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_falls_back_when_user_has_no_whatsapp() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                whatsapp_number: None,
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier = WhatsAppNotifier::with_http_client_and_registry(
            enabled_config(),
            MockWhatsAppClient::success(),
            registry,
        );
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        // Falls back to config to_numbers
        assert_eq!(recipients, vec!["+15559876543".to_string()]);
    }

    #[tokio::test]
    async fn test_ask_question_message_contains_question() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test Issue", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-wa-1".to_string(),
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
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(!body.contains("[CLAUDEAR-Q:"));
        assert!(body.contains("Human input needed for LIN-1"));
        assert!(body.contains("Which branch?"));
    }

    #[tokio::test]
    async fn test_ask_question_delivery_channel_is_whatsapp() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = crate::types::AskRequest {
            correlation_id: "tok-wa-2".to_string(),
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
        assert_eq!(delivery.channel, "whatsapp");
        assert!(delivery.target.is_none());
        assert!(delivery.message_id.is_none());
    }

    #[tokio::test]
    async fn test_notify_start_message_includes_source_and_title() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "1",
            "SEN-42",
            "Memory leak in worker",
            "https://sentry.io/42",
            "sentry",
        );
        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("SEN-42"));
        assert!(body.contains("sentry"));
        assert!(body.contains("Memory leak in worker"));
    }

    #[tokio::test]
    async fn test_notify_failed_short_error_not_truncated() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        notifier.notify_failed(&issue, "Short error").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("Short error"));
        assert!(!body.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_failed_exact_100_char_error_not_truncated() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Test", "https://example.com", "linear");

        let error = "x".repeat(100);
        notifier.notify_failed(&issue, &error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains(&error));
        assert!(!body.ends_with("..."));
    }

    #[tokio::test]
    async fn test_send_message_within_limit_not_truncated() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);

        let message = "x".repeat(100);
        notifier.notify_status(&message).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(!body.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_routes_to_resolved_user_whatsapp_number() {
        let mock = MockWhatsAppClient::success();
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                whatsapp_number: Some("+15550009999".to_string()),
                ..Default::default()
            },
        );
        let registry = crate::users::UserRegistry::new(users);
        let notifier =
            WhatsAppNotifier::with_http_client_and_registry(enabled_config(), mock, registry);
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let to = calls[0].2["to"].as_str().unwrap();
        assert_eq!(to, "+15550009999");
    }

    // --- Tests for cascade success message ---

    #[tokio::test]
    async fn test_notify_success_cascade_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        notifier
            .notify_success(&issue, "https://github.com/downstream/repo/pull/5")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("Cascade PR"));
        assert!(body.contains("LIN-1"));
        assert!(body.contains("downstream/repo"));
        assert!(body.contains("https://github.com/downstream/repo/pull/5"));
    }

    // --- Tests for PR update success message ---

    #[tokio::test]
    async fn test_notify_success_pr_update_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("is_pr_update", true);

        notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/77")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("PR Updated"));
        assert!(body.contains("LIN-1"));
        assert!(body.contains("https://github.com/org/repo/pull/77"));
    }

    // --- Tests for regression resolved completed message ---

    #[tokio::test]
    async fn test_notify_completed_regression_resolved_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_resolved", true);

        notifier.notify_completed(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("Regression Resolved"));
        assert!(body.contains("SEN-1"));
        assert!(body.contains("no regression"));
    }

    // --- Tests for regression detected failed message ---

    #[tokio::test]
    async fn test_notify_failed_regression_detected_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_detected", true);

        notifier
            .notify_failed(&issue, "Tests failing again")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("REGRESSION"));
        assert!(body.contains("SEN-1"));
        assert!(body.contains("Tests failing again"));
    }

    // --- Tests for cascade failed message ---

    #[tokio::test]
    async fn test_notify_failed_cascade_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        notifier.notify_failed(&issue, "Build error").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("CASCADE FAILED"));
        assert!(body.contains("LIN-1"));
        assert!(body.contains("downstream/repo"));
        assert!(body.contains("Build error"));
    }

    // --- Tests for notify_merged and notify_closed ---

    #[tokio::test]
    async fn test_notify_merged_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Fix", "https://example.com", "linear");

        notifier
            .notify_merged(&issue, "https://github.com/org/repo/pull/42")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("PR Merged"));
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("https://github.com/org/repo/pull/42"));
    }

    #[tokio::test]
    async fn test_notify_closed_message_format() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new("1", "PROJ-1", "Fix", "https://example.com", "linear");

        notifier
            .notify_closed(&issue, "https://github.com/org/repo/pull/43")
            .await
            .unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("PR Closed"));
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("https://github.com/org/repo/pull/43"));
    }

    // --- Test failed cascade with long error truncation ---

    #[tokio::test]
    async fn test_notify_failed_cascade_truncates_long_error() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "LIN-1", "Fix", "https://example.com", "linear");
        issue.set_metadata("cascade_downstream_repo", "downstream/repo");

        let long_error = "e".repeat(200);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("CASCADE FAILED"));
        assert!(body.contains("..."));
    }

    // --- Test regression with long error truncation ---

    #[tokio::test]
    async fn test_notify_failed_regression_truncates_long_error() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);
        let mut issue = Issue::new("1", "SEN-1", "Error", "https://sentry.io/1", "sentry");
        issue.set_metadata("regression_detected", true);

        let long_error = "r".repeat(200);
        notifier.notify_failed(&issue, &long_error).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = calls[0].2["text"]["body"].as_str().unwrap();
        assert!(body.contains("REGRESSION"));
        assert!(body.contains("..."));
    }

    // --- Additional test: JSON payload structure ---

    #[tokio::test]
    async fn test_json_payload_has_correct_structure() {
        let mock = MockWhatsAppClient::success();
        let notifier = WhatsAppNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("test message").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let payload = &calls[0].2;

        // Verify all required fields exist
        assert!(payload.get("messaging_product").is_some());
        assert!(payload.get("to").is_some());
        assert!(payload.get("type").is_some());
        assert!(payload.get("text").is_some());
        assert!(payload["text"].get("body").is_some());
    }

    // --- Test http response fields ---

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

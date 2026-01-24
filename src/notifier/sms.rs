//! SMS notifier via Twilio.

use super::Notifier;
use crate::config::SmsConfig;
use crate::error::{Error, Result};
use crate::types::Issue;
use async_trait::async_trait;

/// HTTP response for SMS client.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

/// Trait for HTTP client used by SMS notifier.
#[async_trait]
pub trait SmsHttpClient: Send + Sync {
    async fn post_form(
        &self,
        url: &str,
        auth_user: &str,
        auth_pass: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse>;
}

/// Real HTTP client using reqwest.
pub struct ReqwestSmsClient {
    client: reqwest::Client,
}

impl ReqwestSmsClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestSmsClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SmsHttpClient for ReqwestSmsClient {
    async fn post_form(
        &self,
        url: &str,
        auth_user: &str,
        auth_pass: &str,
        params: &[(&str, &str)],
    ) -> Result<HttpResponse> {
        let response = self
            .client
            .post(url)
            .basic_auth(auth_user, Some(auth_pass))
            .form(params)
            .send()
            .await?;

        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();

        Ok(HttpResponse { status, body })
    }
}

/// SMS notifier that sends notifications via Twilio.
pub struct SmsNotifier<H: SmsHttpClient = ReqwestSmsClient> {
    config: SmsConfig,
    http: H,
}

impl SmsNotifier<ReqwestSmsClient> {
    /// Create a new SMS notifier.
    pub fn new(config: SmsConfig) -> Self {
        Self {
            config,
            http: ReqwestSmsClient::new(),
        }
    }
}

impl<H: SmsHttpClient> SmsNotifier<H> {
    /// Create a new SMS notifier with custom HTTP client.
    pub fn with_http_client(config: SmsConfig, http: H) -> Self {
        Self { config, http }
    }

    async fn send_sms(&self, body: &str) -> Result<()> {
        let (account_sid, auth_token, from_number) = match (
            &self.config.account_sid,
            &self.config.auth_token,
            &self.config.from_number,
        ) {
            (Some(sid), Some(token), Some(from)) => (sid, token, from),
            _ => return Ok(()),
        };

        let url = format!(
            "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
            account_sid
        );

        // Truncate message to SMS limit (160 chars for basic SMS, 1600 for modern)
        let truncated_body = if body.len() > 1500 {
            format!("{}...", &body[..1497])
        } else {
            body.to_string()
        };

        for to_number in &self.config.to_numbers {
            let params = [
                ("From", from_number.as_str()),
                ("To", to_number.as_str()),
                ("Body", &truncated_body),
            ];

            let response = self
                .http
                .post_form(&url, account_sid, auth_token, &params)
                .await?;

            if response.status < 200 || response.status >= 300 {
                return Err(Error::notifier(
                    "sms",
                    format!("Twilio error: {}", response.body),
                ));
            }
        }

        Ok(())
    }
}

#[async_trait]
impl<H: SmsHttpClient + 'static> Notifier for SmsNotifier<H> {
    fn name(&self) -> &str {
        "sms"
    }

    fn is_enabled(&self) -> bool {
        self.config.account_sid.is_some()
            && self.config.auth_token.is_some()
            && self.config.from_number.is_some()
            && !self.config.to_numbers.is_empty()
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let body = format!(
            "[Claude Watchers] Processing {} from {} - {}",
            issue.short_id, issue.source, issue.title
        );
        self.send_sms(&body).await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let body = format!(
            "[Claude Watchers] PR Created for {}: {}",
            issue.short_id, pr_url
        );
        self.send_sms(&body).await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let body = format!("[Claude Watchers] Completed {} (no PR URL)", issue.short_id);
        self.send_sms(&body).await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        // Truncate error for SMS
        let short_error = if error.len() > 100 {
            format!("{}...", &error[..97])
        } else {
            error.to_string()
        };

        let body = format!(
            "[Claude Watchers] FAILED {}: {}",
            issue.short_id, short_error
        );
        self.send_sms(&body).await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let body = format!("[Claude Watchers] {}", message);
        self.send_sms(&body).await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let body = format!(
            "[Claude Watchers] {} urgent issue(s): {}",
            issues.len(),
            issues
                .iter()
                .take(3)
                .map(|i| i.short_id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.send_sms(&body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Mock SMS HTTP client for testing.
    #[allow(clippy::type_complexity)]
    struct MockSmsClient {
        response_status: u16,
        response_body: String,
        call_count: AtomicUsize,
        last_calls: Mutex<Vec<(String, String, String, Vec<(String, String)>)>>,
    }

    impl MockSmsClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response_status: status,
                response_body: body.to_string(),
                call_count: AtomicUsize::new(0),
                last_calls: Mutex::new(Vec::new()),
            }
        }

        fn success() -> Self {
            Self::new(200, r#"{"sid": "SMxxx", "status": "queued"}"#)
        }

        fn error(status: u16, body: &str) -> Self {
            Self::new(status, body)
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }

        #[allow(clippy::type_complexity)]
        fn get_last_calls(&self) -> Vec<(String, String, String, Vec<(String, String)>)> {
            self.last_calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SmsHttpClient for MockSmsClient {
        async fn post_form(
            &self,
            url: &str,
            auth_user: &str,
            auth_pass: &str,
            params: &[(&str, &str)],
        ) -> Result<HttpResponse> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let params_owned: Vec<(String, String)> = params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            self.last_calls.lock().unwrap().push((
                url.to_string(),
                auth_user.to_string(),
                auth_pass.to_string(),
                params_owned,
            ));

            Ok(HttpResponse {
                status: self.response_status,
                body: self.response_body.clone(),
            })
        }
    }

    fn disabled_config() -> SmsConfig {
        SmsConfig {
            account_sid: None,
            auth_token: None,
            from_number: None,
            to_numbers: vec![],
        }
    }

    fn enabled_config() -> SmsConfig {
        SmsConfig {
            account_sid: Some("AC123456".to_string()),
            auth_token: Some("auth_token_xyz".to_string()),
            from_number: Some("+15551234567".to_string()),
            to_numbers: vec!["+15559876543".to_string()],
        }
    }

    fn multi_recipient_config() -> SmsConfig {
        SmsConfig {
            account_sid: Some("AC123456".to_string()),
            auth_token: Some("auth_token_xyz".to_string()),
            from_number: Some("+15551234567".to_string()),
            to_numbers: vec![
                "+15551111111".to_string(),
                "+15552222222".to_string(),
                "+15553333333".to_string(),
            ],
        }
    }

    fn partial_config_no_sid() -> SmsConfig {
        SmsConfig {
            account_sid: None,
            auth_token: Some("token".to_string()),
            from_number: Some("+1234567890".to_string()),
            to_numbers: vec!["+0987654321".to_string()],
        }
    }

    fn partial_config_no_token() -> SmsConfig {
        SmsConfig {
            account_sid: Some("sid".to_string()),
            auth_token: None,
            from_number: Some("+1234567890".to_string()),
            to_numbers: vec!["+0987654321".to_string()],
        }
    }

    fn partial_config_no_from() -> SmsConfig {
        SmsConfig {
            account_sid: Some("sid".to_string()),
            auth_token: Some("token".to_string()),
            from_number: None,
            to_numbers: vec!["+0987654321".to_string()],
        }
    }

    fn partial_config_no_to() -> SmsConfig {
        SmsConfig {
            account_sid: Some("sid".to_string()),
            auth_token: Some("token".to_string()),
            from_number: Some("+1234567890".to_string()),
            to_numbers: vec![],
        }
    }

    #[test]
    fn test_is_enabled() {
        let enabled_config = SmsConfig {
            account_sid: Some("test".to_string()),
            auth_token: Some("test".to_string()),
            from_number: Some("+1234567890".to_string()),
            to_numbers: vec!["+0987654321".to_string()],
        };
        let notifier = SmsNotifier::new(enabled_config);
        assert!(notifier.is_enabled());

        let disabled_config = SmsConfig {
            account_sid: None,
            auth_token: None,
            from_number: None,
            to_numbers: vec![],
        };
        let notifier = SmsNotifier::new(disabled_config);
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_name() {
        let notifier = SmsNotifier::new(disabled_config());
        assert_eq!(notifier.name(), "sms");
    }

    #[test]
    fn test_is_enabled_partial_configs() {
        assert!(!SmsNotifier::new(partial_config_no_sid()).is_enabled());
        assert!(!SmsNotifier::new(partial_config_no_token()).is_enabled());
        assert!(!SmsNotifier::new(partial_config_no_from()).is_enabled());
        assert!(!SmsNotifier::new(partial_config_no_to()).is_enabled());
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let notifier = SmsNotifier::new(disabled_config());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let notifier = SmsNotifier::new(disabled_config());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let notifier = SmsNotifier::new(disabled_config());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let notifier = SmsNotifier::new(disabled_config());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_failed(&issue, "Error message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_long_error() {
        let notifier = SmsNotifier::new(disabled_config());
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        // Error longer than 100 characters
        let long_error = "x".repeat(200);
        let result = notifier.notify_failed(&issue, &long_error).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let notifier = SmsNotifier::new(disabled_config());

        let result = notifier.notify_status("Status update").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let notifier = SmsNotifier::new(disabled_config());

        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_disabled() {
        let notifier = SmsNotifier::new(disabled_config());
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncated_to_three() {
        let notifier = SmsNotifier::new(disabled_config());
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
        let config = SmsConfig {
            account_sid: Some("sid".to_string()),
            auth_token: Some("token".to_string()),
            from_number: Some("+1234567890".to_string()),
            to_numbers: vec![
                "+1111111111".to_string(),
                "+2222222222".to_string(),
                "+3333333333".to_string(),
            ],
        };

        let notifier = SmsNotifier::new(config);
        assert!(notifier.is_enabled());
    }

    // Mock-based tests for HTTP-dependent functionality

    #[tokio::test]
    async fn test_send_sms_success() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
    async fn test_send_sms_verifies_url_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        assert!(calls[0].0.contains("api.twilio.com"));
        assert!(calls[0].0.contains("AC123456")); // Account SID in URL
        assert!(calls[0].0.contains("Messages.json"));
    }

    #[tokio::test]
    async fn test_send_sms_uses_basic_auth() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        assert_eq!(calls[0].1, "AC123456"); // auth_user
        assert_eq!(calls[0].2, "auth_token_xyz"); // auth_pass
    }

    #[tokio::test]
    async fn test_send_sms_sends_correct_params() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_start(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let params = &calls[0].3;
        assert!(params
            .iter()
            .any(|(k, v)| k == "From" && v == "+15551234567"));
        assert!(params.iter().any(|(k, v)| k == "To" && v == "+15559876543"));
        assert!(params
            .iter()
            .any(|(k, v)| k == "Body" && v.contains("Processing")));
    }

    #[tokio::test]
    async fn test_send_sms_multiple_recipients() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(multi_recipient_config(), mock);
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
    async fn test_send_sms_error_response() {
        let mock = MockSmsClient::error(400, "Invalid phone number");
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        assert!(err_str.contains("Twilio error"));
        assert!(err_str.contains("Invalid phone number"));
    }

    #[tokio::test]
    async fn test_send_sms_server_error() {
        let mock = MockSmsClient::error(500, "Internal server error");
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
    async fn test_send_sms_truncates_long_message() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);

        // Create a message longer than 1500 chars
        let long_message = "x".repeat(2000);
        notifier.notify_status(&long_message).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body_param = calls[0].3.iter().find(|(k, _)| k == "Body").unwrap();
        // Body should be truncated to 1500 chars + "..."
        assert!(body_param.1.len() <= 1600); // "[Claude Watchers] " + truncated body
        assert!(body_param.1.ends_with("..."));
    }

    #[tokio::test]
    async fn test_notify_success_message_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert!(body.contains("[Claude Watchers]"));
        assert!(body.contains("PR Created"));
        assert!(body.contains("PROJ-123"));
        assert!(body.contains("https://github.com/org/repo/pull/42"));
    }

    #[tokio::test]
    async fn test_notify_completed_message_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );

        notifier.notify_completed(&issue).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert!(body.contains("Completed"));
        assert!(body.contains("no PR URL"));
    }

    #[tokio::test]
    async fn test_notify_failed_message_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert!(body.contains("FAILED"));
        assert!(body.contains("PROJ-123"));
        assert!(body.contains("Build failed"));
    }

    #[tokio::test]
    async fn test_notify_failed_truncates_long_error() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        // Error should be truncated to 100 chars including "..."
        assert!(body.contains("..."));
    }

    #[tokio::test]
    async fn test_notify_status_message_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);

        notifier.notify_status("System is healthy").await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert_eq!(body, "[Claude Watchers] System is healthy");
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_message_format() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        notifier.notify_urgent_issues(&issues).await.unwrap();

        let calls = notifier.http.get_last_calls();
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert!(body.contains("2 urgent issue(s)"));
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("PROJ-2"));
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_truncates_to_three() {
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);
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
        let body = &calls[0].3.iter().find(|(k, _)| k == "Body").unwrap().1;
        assert!(body.contains("10 urgent issue(s)"));
        // Only first 3 are listed
        assert!(body.contains("PROJ-1"));
        assert!(body.contains("PROJ-2"));
        assert!(body.contains("PROJ-3"));
        assert!(!body.contains("PROJ-4"));
    }

    #[tokio::test]
    async fn test_send_sms_stops_on_first_error() {
        let mock = MockSmsClient::error(400, "Bad request");
        let notifier = SmsNotifier::with_http_client(multi_recipient_config(), mock);
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
        let mock = MockSmsClient::success();
        let notifier = SmsNotifier::with_http_client(enabled_config(), mock);

        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "sms");
    }

    #[test]
    fn test_reqwest_sms_client_default() {
        let client = ReqwestSmsClient::default();
        // Just verify it can be constructed
        assert!(std::mem::size_of_val(&client) > 0);
    }

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

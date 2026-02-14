//! Email notifier via SMTP.

use super::Notifier;
use crate::config::EmailConfig;
use crate::error::{Error, Result};
use crate::types::Issue;
use crate::users::UserRegistry;
use async_trait::async_trait;
use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};

/// Email notifier that sends notifications via SMTP.
pub struct EmailNotifier {
    config: EmailConfig,
    transport: Option<AsyncSmtpTransport<Tokio1Executor>>,
    user_registry: UserRegistry,
}

impl EmailNotifier {
    /// Create a new email notifier.
    pub fn new(config: EmailConfig, user_registry: UserRegistry) -> Result<Self> {
        let transport = if let (Some(host), Some(username), Some(password)) = (
            config.smtp_host.as_ref(),
            config.smtp_username.as_ref(),
            config.smtp_password.as_ref(),
        ) {
            let creds = Credentials::new(username.clone(), password.clone());

            let builder = if config.use_tls {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
                    .map_err(|e| Error::notifier("email", format!("SMTP setup error: {}", e)))?
            } else {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            };

            Some(builder.port(config.smtp_port).credentials(creds).build())
        } else {
            None
        };

        Ok(Self {
            config,
            transport,
            user_registry,
        })
    }

    fn resolve_recipients(&self, issue: Option<&Issue>) -> Vec<String> {
        if let Some(issue) = issue {
            if let Some(slug) = issue.get_metadata::<String>("resolved_user") {
                if let Some(user) = self.user_registry.get_by_slug(&slug) {
                    if let Some(ref email) = user.email {
                        return vec![email.clone()];
                    }
                }
            }
        }
        self.config.to_addresses.clone()
    }

    async fn send_email(&self, subject: &str, body: &str, issue: Option<&Issue>) -> Result<()> {
        let transport = match &self.transport {
            Some(t) => t,
            None => return Ok(()),
        };

        let from_address = match &self.config.from_address {
            Some(addr) => addr
                .parse::<Mailbox>()
                .map_err(|e| Error::notifier("email", format!("Invalid from address: {}", e)))?,
            None => return Ok(()),
        };

        let recipients = self.resolve_recipients(issue);

        for to_address in &recipients {
            let to_mailbox = to_address
                .parse::<Mailbox>()
                .map_err(|e| Error::notifier("email", format!("Invalid to address: {}", e)))?;

            let message = Message::builder()
                .from(from_address.clone())
                .to(to_mailbox)
                .subject(subject)
                .header(ContentType::TEXT_PLAIN)
                .body(body.to_string())
                .map_err(|e| Error::notifier("email", format!("Failed to build email: {}", e)))?;

            transport
                .send(message)
                .await
                .map_err(|e| Error::notifier("email", format!("Failed to send email: {}", e)))?;
        }

        Ok(())
    }

    fn format_issue_info(issue: &Issue) -> String {
        format!(
            "Issue: {} - {}\nSource: {}\nPriority: {}\nStatus: {}\nURL: {}",
            issue.short_id, issue.title, issue.source, issue.priority, issue.status, issue.url
        )
    }
}

#[async_trait]
impl Notifier for EmailNotifier {
    fn name(&self) -> &str {
        "email"
    }

    fn is_enabled(&self) -> bool {
        self.transport.is_some()
            && self.config.from_address.is_some()
            && !self.config.to_addresses.is_empty()
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let subject = format!("[Claude Watchers] Processing: {}", issue.short_id);
        let body = format!(
            "Claude Watchers is now processing an issue.\n\n{}\n\nYou will receive another notification when processing completes.",
            Self::format_issue_info(issue)
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let subject = format!("[Claude Watchers] PR Created: {}", issue.short_id);
        let body = format!(
            "Claude Watchers successfully created a PR!\n\n{}\n\nPR URL: {}",
            Self::format_issue_info(issue),
            pr_url
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let subject = format!("[Claude Watchers] Completed: {}", issue.short_id);
        let body = format!(
            "Claude Watchers completed processing but no PR URL was captured.\n\n{}",
            Self::format_issue_info(issue)
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let subject = format!("[Claude Watchers] Failed: {}", issue.short_id);
        let body = format!(
            "Claude Watchers failed to process an issue.\n\n{}\n\nError: {}",
            Self::format_issue_info(issue),
            error
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let subject = "[Claude Watchers] Status Update".to_string();
        self.send_email(&subject, message, None).await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let subject = format!(
            "[Claude Watchers] {} Urgent Issue{} Detected",
            issues.len(),
            if issues.len() > 1 { "s" } else { "" }
        );

        let mut body = "The following urgent issues require attention:\n\n".to_string();
        for issue in issues.iter().take(10) {
            body.push_str(&format!(
                "- [{}] {} - {}\n  URL: {}\n\n",
                issue.source, issue.short_id, issue.title, issue.url
            ));
        }

        self.send_email(&subject, &body, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IssuePriority, IssueStatus};

    fn empty_registry() -> UserRegistry {
        UserRegistry::new(std::collections::HashMap::new())
    }

    fn disabled_config() -> EmailConfig {
        EmailConfig {
            smtp_host: None,
            smtp_port: 587,
            smtp_username: None,
            smtp_password: None,
            from_address: None,
            to_addresses: vec![],
            use_tls: true,
        }
    }

    fn partial_config() -> EmailConfig {
        EmailConfig {
            smtp_host: Some("smtp.example.com".to_string()),
            smtp_port: 587,
            smtp_username: Some("user".to_string()),
            smtp_password: Some("pass".to_string()),
            from_address: None, // Missing from address
            to_addresses: vec!["test@example.com".to_string()],
            use_tls: true,
        }
    }

    fn no_to_config() -> EmailConfig {
        EmailConfig {
            smtp_host: Some("smtp.example.com".to_string()),
            smtp_port: 587,
            smtp_username: Some("user".to_string()),
            smtp_password: Some("pass".to_string()),
            from_address: Some("from@example.com".to_string()),
            to_addresses: vec![], // No recipients
            use_tls: true,
        }
    }

    #[test]
    fn test_new_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        assert_eq!(notifier.name(), "email");
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_name() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        assert_eq!(notifier.name(), "email");
    }

    #[test]
    fn test_is_enabled_no_transport() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_no_from() {
        let notifier = EmailNotifier::new(partial_config(), empty_registry()).unwrap();
        // Has transport but no from address
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_is_enabled_no_to() {
        let notifier = EmailNotifier::new(no_to_config(), empty_registry()).unwrap();
        // Has transport and from but no recipients
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_format_issue_info() {
        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://example.com",
            "linear",
        );
        issue.priority = IssuePriority::High;
        issue.status = IssueStatus::Open;

        let info = EmailNotifier::format_issue_info(&issue);

        assert!(info.contains("PROJ-123"));
        assert!(info.contains("Test Issue"));
        assert!(info.contains("linear"));
        assert!(info.contains("https://example.com"));
    }

    #[test]
    fn test_format_issue_info_all_priorities() {
        for priority in [
            IssuePriority::Critical,
            IssuePriority::High,
            IssuePriority::Medium,
            IssuePriority::Low,
            IssuePriority::None,
        ] {
            let mut issue = Issue::new("123", "TEST-1", "Test", "https://example.com", "linear");
            issue.priority = priority;

            let info = EmailNotifier::format_issue_info(&issue);
            assert!(!info.is_empty());
        }
    }

    #[test]
    fn test_format_issue_info_all_statuses() {
        for status in [
            IssueStatus::Open,
            IssueStatus::InProgress,
            IssueStatus::Resolved,
            IssueStatus::Ignored,
        ] {
            let mut issue = Issue::new("123", "TEST-1", "Test", "https://example.com", "sentry");
            issue.status = status;

            let info = EmailNotifier::format_issue_info(&issue);
            assert!(!info.is_empty());
        }
    }

    #[tokio::test]
    async fn test_notify_start_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        // Should return Ok even when disabled
        let result = notifier.notify_start(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_success_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier
            .notify_success(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_completed_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_completed(&issue).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_failed_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        let issue = Issue::new("123", "PROJ-123", "Test", "https://example.com", "linear");

        let result = notifier.notify_failed(&issue, "Error message").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_status_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();

        let result = notifier.notify_status("Status update").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_empty() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();

        let result = notifier.notify_urgent_issues(&[]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_disabled() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();
        let issues = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];

        let result = notifier.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_notify_urgent_issues_single_vs_plural() {
        let notifier = EmailNotifier::new(disabled_config(), empty_registry()).unwrap();

        // Single issue
        let single = vec![Issue::new(
            "1",
            "PROJ-1",
            "Issue",
            "https://example.com",
            "linear",
        )];
        let result = notifier.notify_urgent_issues(&single).await;
        assert!(result.is_ok());

        // Multiple issues
        let multiple = vec![
            Issue::new("1", "PROJ-1", "Issue 1", "https://example.com", "linear"),
            Issue::new("2", "PROJ-2", "Issue 2", "https://example.com", "linear"),
        ];
        let result = notifier.notify_urgent_issues(&multiple).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_new_with_non_tls() {
        let config = EmailConfig {
            smtp_host: Some("localhost".to_string()),
            smtp_port: 25,
            smtp_username: Some("user".to_string()),
            smtp_password: Some("pass".to_string()),
            from_address: Some("test@localhost".to_string()),
            to_addresses: vec!["recipient@localhost".to_string()],
            use_tls: false,
        };

        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        // Should create successfully with dangerous builder
        assert_eq!(notifier.name(), "email");
    }
}

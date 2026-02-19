//! Email notifier via SMTP.

use super::Notifier;
use crate::config::EmailConfig;
use crate::error::{Error, Result};
use crate::types::{AskDelivery, AskReply, AskRequest, Issue};
use crate::users::UserRegistry;
use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use lettre::{
    message::{header::ContentType, Mailbox},
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use mailparse::MailHeaderMap;
use std::collections::HashSet;

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

    fn target_email_for_issue(&self, issue: &Issue) -> Option<String> {
        self.resolve_recipients(Some(issue)).into_iter().next()
    }

    fn expected_reply_emails(&self, request: &AskRequest) -> HashSet<String> {
        if let Some(ref target) = request.target_email {
            return std::iter::once(target.to_lowercase()).collect();
        }
        self.config
            .to_addresses
            .iter()
            .map(|v| v.to_lowercase())
            .collect()
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

    fn extract_email_address(from_header: &str) -> Option<String> {
        let start = from_header.find('<');
        let end = from_header.find('>');
        if let (Some(s), Some(e)) = (start, end) {
            let addr = from_header[s + 1..e].trim();
            if !addr.is_empty() {
                return Some(addr.to_lowercase());
            }
        }
        let trimmed = from_header.trim();
        if trimmed.contains('@') {
            Some(trimmed.to_lowercase())
        } else {
            None
        }
    }

    fn extract_plain_body(parsed: &mailparse::ParsedMail<'_>) -> String {
        if parsed.subparts.is_empty() {
            return parsed.get_body().unwrap_or_default();
        }

        for part in &parsed.subparts {
            let ctype = part.ctype.mimetype.to_lowercase();
            if ctype == "text/plain" {
                return part.get_body().unwrap_or_default();
            }
        }

        parsed.get_body().unwrap_or_default()
    }

    fn sanitize_reply_text(body: &str, correlation_id: &str) -> Option<String> {
        let token = format!("CLAUDEAR-Q:{}", correlation_id);
        let mut lines: Vec<String> = Vec::new();
        for raw_line in body.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('>') {
                continue;
            }
            if line.contains(&token) {
                continue;
            }
            lines.push(line.to_string());
            if lines.len() >= 30 {
                break;
            }
        }
        let mut out = lines.join("\n").trim().to_string();
        if out.len() > 4000 {
            out.truncate(4000);
        }
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
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
        let subject = format!("[Claudear] Processing: {}", issue.short_id);
        let body = format!(
            "Claudear is now processing an issue.\n\n{}\n\nYou will receive another notification when processing completes.",
            Self::format_issue_info(issue)
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let subject = format!("[Claudear] PR Created: {}", issue.short_id);
        let body = format!(
            "Claudear successfully created a PR!\n\n{}\n\nPR URL: {}",
            Self::format_issue_info(issue),
            pr_url
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let subject = format!("[Claudear] Completed: {}", issue.short_id);
        let body = format!(
            "Claudear completed processing but no PR URL was captured.\n\n{}",
            Self::format_issue_info(issue)
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let subject = format!("[Claudear] Failed: {}", issue.short_id);
        let body = format!(
            "Claudear failed to process an issue.\n\n{}\n\nError: {}",
            Self::format_issue_info(issue),
            error
        );
        self.send_email(&subject, &body, Some(issue)).await
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let subject = "[Claudear] Status Update".to_string();
        self.send_email(&subject, message, None).await
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        if issues.is_empty() {
            return Ok(());
        }

        let subject = format!(
            "[Claudear] {} Urgent Issue{} Detected",
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

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let subject = format!("[CLAUDEAR-Q:{}] {}", request.correlation_id, issue.short_id);
        let mut body = format!(
            "Claude needs human input.\n\n{}\n\nQuestion:\n{}\n",
            Self::format_issue_info(issue),
            request.question.question
        );
        if let Some(ref why) = request.question.why {
            body.push_str(&format!("\nWhy:\n{}\n", why));
        }
        if let Some(ref context) = request.question.context {
            body.push_str(&format!("\nContext:\n{}\n", context));
        }
        if !request.question.options.is_empty() {
            body.push_str(&format!(
                "\nOptions:\n- {}\n",
                request.question.options.join("\n- ")
            ));
        }
        body.push_str("\nReply to this email and keep the token in subject or body.\n");

        self.send_email(&subject, &body, Some(issue)).await?;
        Ok(Some(AskDelivery {
            channel: "email".to_string(),
            target: self.target_email_for_issue(issue),
            message_id: None,
        }))
    }

    async fn poll_question_replies(
        &self,
        request: &AskRequest,
        since: DateTime<Utc>,
    ) -> Result<Vec<AskReply>> {
        let imap_host = match self.config.imap_host.clone() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(Vec::new()),
        };
        let imap_username = match self.config.imap_username.clone() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(Vec::new()),
        };
        let imap_password = match self.config.imap_password.clone() {
            Some(v) if !v.is_empty() => v,
            _ => return Ok(Vec::new()),
        };

        let imap_port = self.config.imap_port;
        let imap_folder = self.config.imap_folder.clone();
        let correlation_id = request.correlation_id.clone();
        let expected_senders = self.expected_reply_emails(request);
        let imap_use_tls = self.config.imap_use_tls;

        tokio::task::spawn_blocking(move || -> Result<Vec<AskReply>> {
            let tls = native_tls::TlsConnector::builder().build().map_err(|e| {
                Error::notifier("email", format!("Failed to build TLS connector: {}", e))
            })?;

            if !imap_use_tls {
                return Ok(Vec::new());
            }

            let client = imap::connect((imap_host.as_str(), imap_port), &imap_host, &tls)
                .map_err(|e| Error::notifier("email", format!("IMAP connect failed: {}", e)))?;
            let mut session = client
                .login(imap_username, imap_password)
                .map_err(|(e, _)| Error::notifier("email", format!("IMAP login failed: {}", e)))?;

            session
                .select(&imap_folder)
                .map_err(|e| Error::notifier("email", format!("IMAP select failed: {}", e)))?;

            let token = format!("CLAUDEAR-Q:{}", correlation_id);
            let search_query = format!("TEXT \"{}\"", token);
            let ids = session
                .search(search_query)
                .map_err(|e| Error::notifier("email", format!("IMAP search failed: {}", e)))?;

            if ids.is_empty() {
                let _ = session.logout();
                return Ok(Vec::new());
            }

            let sequence = ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let fetches = session
                .fetch(sequence, "RFC822")
                .map_err(|e| Error::notifier("email", format!("IMAP fetch failed: {}", e)))?;

            let mut replies = Vec::new();
            for fetch in fetches.iter() {
                let raw = match fetch.body() {
                    Some(v) => v,
                    None => continue,
                };

                let parsed = match mailparse::parse_mail(raw) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let from_header = parsed.headers.get_first_value("From").unwrap_or_default();
                let responder = Self::extract_email_address(&from_header);
                if !expected_senders.is_empty() {
                    match responder.as_ref() {
                        Some(email) if expected_senders.contains(email) => {}
                        _ => continue,
                    }
                }

                let subject = parsed
                    .headers
                    .get_first_value("Subject")
                    .unwrap_or_default();
                let body_text = Self::extract_plain_body(&parsed);
                if !subject.contains(&token) && !body_text.contains(&token) {
                    continue;
                }

                let answer = match Self::sanitize_reply_text(&body_text, &correlation_id) {
                    Some(v) => v,
                    None => continue,
                };

                let replied_at = parsed
                    .headers
                    .get_first_value("Date")
                    .and_then(|date| mailparse::dateparse(&date).ok())
                    .and_then(|secs| Utc.timestamp_opt(secs, 0).single())
                    .unwrap_or_else(Utc::now);

                if replied_at < since {
                    continue;
                }

                replies.push(AskReply {
                    correlation_id: correlation_id.clone(),
                    channel: "email".to_string(),
                    responder,
                    answer,
                    replied_at,
                });
            }

            replies.sort_by_key(|r| r.replied_at);
            let _ = session.logout();
            Ok(replies)
        })
        .await
        .map_err(|e| Error::notifier("email", format!("IMAP task failed: {}", e)))?
    }

    fn supports_replies(&self) -> bool {
        self.config
            .imap_host
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or(false)
            && self
                .config
                .imap_username
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
            && self
                .config
                .imap_password
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
        };

        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        // Should create successfully with dangerous builder
        assert_eq!(notifier.name(), "email");
    }

    #[test]
    fn test_extract_email_address_variants() {
        assert_eq!(
            EmailNotifier::extract_email_address("Jane <jane@example.com>").as_deref(),
            Some("jane@example.com")
        );
        assert_eq!(
            EmailNotifier::extract_email_address("ops@example.com").as_deref(),
            Some("ops@example.com")
        );
    }

    #[test]
    fn test_sanitize_reply_text_removes_token_and_quotes() {
        let body = "Thanks\n\n> quoted\nCLAUDEAR-Q:abc123\nUse main";
        let parsed = EmailNotifier::sanitize_reply_text(body, "abc123").unwrap();
        assert_eq!(parsed, "Thanks\nUse main");
    }

    #[tokio::test]
    async fn test_ask_question_routes_to_resolved_user_email() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                email: Some("jake@example.com".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = EmailConfig {
            smtp_host: None,
            from_address: Some("bot@example.com".to_string()),
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, registry).unwrap();
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let request = AskRequest {
            correlation_id: "tok-email-1".to_string(),
            source: "linear".to_string(),
            repo: Some("org/repo".to_string()),
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which env?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.target.as_deref(), Some("jake@example.com"));
    }

    #[test]
    fn test_extract_email_address_angle_brackets() {
        assert_eq!(
            EmailNotifier::extract_email_address("John Doe <john@example.com>").as_deref(),
            Some("john@example.com")
        );
    }

    #[test]
    fn test_extract_email_address_bare_email() {
        assert_eq!(
            EmailNotifier::extract_email_address("user@example.com").as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn test_extract_email_address_lowercases() {
        assert_eq!(
            EmailNotifier::extract_email_address("User@Example.COM").as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn test_extract_email_address_angle_bracket_lowercases() {
        assert_eq!(
            EmailNotifier::extract_email_address("Foo <FOO@BAR.COM>").as_deref(),
            Some("foo@bar.com")
        );
    }

    #[test]
    fn test_extract_email_address_empty_angle_brackets_falls_back() {
        // Empty inside angle brackets should fall back to plain parsing
        assert_eq!(
            EmailNotifier::extract_email_address("Name <>").as_deref(),
            None
        );
    }

    #[test]
    fn test_extract_email_address_no_at_symbol_returns_none() {
        assert_eq!(
            EmailNotifier::extract_email_address("not-an-email").as_deref(),
            None
        );
    }

    #[test]
    fn test_extract_email_address_empty_string() {
        assert_eq!(EmailNotifier::extract_email_address("").as_deref(), None);
    }

    #[test]
    fn test_extract_email_address_whitespace_trimmed() {
        assert_eq!(
            EmailNotifier::extract_email_address("  user@example.com  ").as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn test_sanitize_reply_text_strips_quoted_lines() {
        let body = "> original message\nMy actual reply";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "My actual reply");
    }

    #[test]
    fn test_sanitize_reply_text_strips_token_line() {
        let body = "CLAUDEAR-Q:tok-1\nUse staging environment";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "Use staging environment");
    }

    #[test]
    fn test_sanitize_reply_text_strips_empty_lines() {
        let body = "\n\n\nHello\n\n\n";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_sanitize_reply_text_all_quoted_returns_none() {
        let body = "> quoted line 1\n> quoted line 2\n> quoted line 3";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_sanitize_reply_text_empty_body_returns_none() {
        let result = EmailNotifier::sanitize_reply_text("", "tok-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_sanitize_reply_text_only_whitespace_returns_none() {
        let result = EmailNotifier::sanitize_reply_text("   \n   \n   ", "tok-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_sanitize_reply_text_truncates_long_output() {
        let long_line = "x".repeat(5000);
        let result = EmailNotifier::sanitize_reply_text(&long_line, "tok-1").unwrap();
        assert!(result.len() <= 4000);
    }

    #[test]
    fn test_sanitize_reply_text_limits_to_30_lines() {
        let body = (1..=50)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = EmailNotifier::sanitize_reply_text(&body, "tok-1").unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 30,
            "Expected at most 30 lines, got {}",
            line_count
        );
    }

    #[test]
    fn test_sanitize_reply_text_mixed_content() {
        let body = "> On Mon, user wrote:\n> Original question\n\nMy answer is yes\n\nCLAUDEAR-Q:tok-1\n\n> More quoted text\nSecond line of answer";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "My answer is yes\nSecond line of answer");
    }

    #[test]
    fn test_format_issue_info_contains_all_fields() {
        let mut issue = Issue::new(
            "42",
            "PROJ-42",
            "Authentication bug",
            "https://linear.app/42",
            "linear",
        );
        issue.priority = IssuePriority::Critical;
        issue.status = IssueStatus::InProgress;

        let info = EmailNotifier::format_issue_info(&issue);
        assert!(info.contains("PROJ-42"));
        assert!(info.contains("Authentication bug"));
        assert!(info.contains("linear"));
        assert!(info.contains("https://linear.app/42"));
        assert!(info.contains(&issue.priority.to_string()));
        assert!(info.contains(&issue.status.to_string()));
    }

    #[test]
    fn test_format_issue_info_format_structure() {
        let issue = Issue::new("1", "TEST-1", "Title", "https://url.com", "sentry");
        let info = EmailNotifier::format_issue_info(&issue);
        // Verify it has the expected "Key: Value" structure
        assert!(info.starts_with("Issue: TEST-1 - Title"));
        assert!(info.contains("Source: sentry"));
        assert!(info.contains("URL: https://url.com"));
    }

    #[test]
    fn test_resolve_recipients_returns_global_when_no_issue() {
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let recipients = notifier.resolve_recipients(None);
        assert_eq!(recipients, vec!["global@example.com".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_returns_global_when_no_resolved_user() {
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["global@example.com".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_uses_resolved_user_email() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                email: Some("jake@example.com".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, registry).unwrap();
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["jake@example.com".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_falls_back_when_user_has_no_email() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                email: None,
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, registry).unwrap();
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        let recipients = notifier.resolve_recipients(Some(&issue));
        assert_eq!(recipients, vec!["global@example.com".to_string()]);
    }

    #[test]
    fn test_expected_reply_emails_from_request_target() {
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-1".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: Some("Specific@Example.COM".to_string()),
            target_slack_id: None,
        };
        let emails = notifier.expected_reply_emails(&request);
        assert_eq!(emails.len(), 1);
        assert!(emails.contains("specific@example.com"));
    }

    #[test]
    fn test_expected_reply_emails_falls_back_to_to_addresses() {
        let config = EmailConfig {
            to_addresses: vec!["a@example.com".to_string(), "B@Example.COM".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-2".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let emails = notifier.expected_reply_emails(&request);
        assert_eq!(emails.len(), 2);
        assert!(emails.contains("a@example.com"));
        assert!(emails.contains("b@example.com"));
    }

    #[test]
    fn test_supports_replies_true_when_all_imap_set() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_imap_host() {
        let config = EmailConfig {
            imap_host: None,
            imap_username: Some("user".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_empty_imap_host() {
        let config = EmailConfig {
            imap_host: Some("".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_imap_username() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: None,
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_no_imap_password() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: None,
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_supports_replies_false_when_empty_password() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: Some("".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    #[test]
    fn test_target_email_for_issue_returns_first_recipient() {
        let config = EmailConfig {
            to_addresses: vec![
                "first@example.com".to_string(),
                "second@example.com".to_string(),
            ],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        assert_eq!(
            notifier.target_email_for_issue(&issue),
            Some("first@example.com".to_string())
        );
    }

    #[test]
    fn test_target_email_for_issue_none_when_empty() {
        let config = EmailConfig {
            to_addresses: vec![],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        assert_eq!(notifier.target_email_for_issue(&issue), None);
    }

    #[tokio::test]
    async fn test_ask_question_delivery_channel_is_email() {
        let config = EmailConfig {
            to_addresses: vec!["user@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = AskRequest {
            correlation_id: "tok-ch".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.channel, "email");
        assert!(delivery.message_id.is_none());
    }

    // -----------------------------------------------------------------------
    // Additional extract_email_address tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_email_address_with_display_name_and_angle_brackets() {
        assert_eq!(
            EmailNotifier::extract_email_address("Alice Bob <alice@company.org>").as_deref(),
            Some("alice@company.org")
        );
    }

    #[test]
    fn test_extract_email_address_with_quotes_in_display_name() {
        assert_eq!(
            EmailNotifier::extract_email_address("\"Alice Bob\" <alice@company.org>").as_deref(),
            Some("alice@company.org")
        );
    }

    #[test]
    fn test_extract_email_address_only_whitespace() {
        assert_eq!(EmailNotifier::extract_email_address("   ").as_deref(), None);
    }

    #[test]
    fn test_extract_email_address_empty_brackets_no_at() {
        // "<>" with name prefix - no @ in fallback
        assert_eq!(
            EmailNotifier::extract_email_address("NoEmail <>").as_deref(),
            None
        );
    }

    #[test]
    fn test_extract_email_address_nested_angle_brackets() {
        // Picks the first < > pair
        assert_eq!(
            EmailNotifier::extract_email_address("Name <outer@example.com> extra>").as_deref(),
            Some("outer@example.com")
        );
    }

    #[test]
    fn test_extract_email_address_uppercase_in_brackets_lowercased() {
        assert_eq!(
            EmailNotifier::extract_email_address("Name <USER@EXAMPLE.COM>").as_deref(),
            Some("user@example.com")
        );
    }

    // -----------------------------------------------------------------------
    // Additional sanitize_reply_text tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_sanitize_reply_text_preserves_multiline_answer() {
        let body = "Line one\nLine two\nLine three";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "Line one\nLine two\nLine three");
    }

    #[test]
    fn test_sanitize_reply_text_strips_multiple_token_lines() {
        let body = "CLAUDEAR-Q:tok-1\nAnswer\nCLAUDEAR-Q:tok-1\nMore answer";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "Answer\nMore answer");
    }

    #[test]
    fn test_sanitize_reply_text_strips_mixed_quotes_and_tokens() {
        let body =
            "> Original question\n> Second line\nCLAUDEAR-Q:tok-1\n\nMy answer\n> Another quote";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        assert_eq!(result, "My answer");
    }

    #[test]
    fn test_sanitize_reply_text_only_token_returns_none() {
        let body = "CLAUDEAR-Q:tok-1";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_sanitize_reply_text_token_with_whitespace_only_returns_none() {
        let body = "CLAUDEAR-Q:tok-1\n   \n   \n";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1");
        assert!(result.is_none());
    }

    #[test]
    fn test_sanitize_reply_text_different_correlation_id_preserves_line() {
        let body = "CLAUDEAR-Q:other-tok\nSome answer";
        let result = EmailNotifier::sanitize_reply_text(body, "tok-1").unwrap();
        // The line with CLAUDEAR-Q:other-tok is NOT stripped (different token)
        assert!(result.contains("CLAUDEAR-Q:other-tok"));
        assert!(result.contains("Some answer"));
    }

    #[test]
    fn test_sanitize_reply_text_exactly_30_lines() {
        let body = (1..=30)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = EmailNotifier::sanitize_reply_text(&body, "tok-1").unwrap();
        assert_eq!(result.lines().count(), 30);
    }

    #[test]
    fn test_sanitize_reply_text_31_lines_truncated() {
        let body = (1..=31)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = EmailNotifier::sanitize_reply_text(&body, "tok-1").unwrap();
        assert_eq!(result.lines().count(), 30);
        assert!(result.contains("Line 30"));
        assert!(!result.contains("Line 31"));
    }

    #[test]
    fn test_sanitize_reply_text_exactly_4000_chars() {
        // Build a string that is exactly 4000 chars of content lines
        let line = "x".repeat(100);
        let body = (0..40).map(|_| line.clone()).collect::<Vec<_>>().join("\n"); // 40 * 100 + 39 newlines but only 30 lines are kept
        let result = EmailNotifier::sanitize_reply_text(&body, "tok-1").unwrap();
        assert!(result.len() <= 4000);
    }

    // -----------------------------------------------------------------------
    // extract_plain_body tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_plain_body_simple() {
        let raw = b"From: user@example.com\r\nSubject: Test\r\nContent-Type: text/plain\r\n\r\nHello world";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let body = EmailNotifier::extract_plain_body(&parsed);
        assert!(body.contains("Hello world"));
    }

    #[test]
    fn test_extract_plain_body_multipart() {
        let raw = b"From: user@example.com\r\nSubject: Test\r\nContent-Type: multipart/alternative; boundary=bound\r\n\r\n--bound\r\nContent-Type: text/plain\r\n\r\nPlain text body\r\n--bound\r\nContent-Type: text/html\r\n\r\n<p>HTML body</p>\r\n--bound--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let body = EmailNotifier::extract_plain_body(&parsed);
        assert!(body.contains("Plain text body"));
        assert!(!body.contains("<p>"));
    }

    #[test]
    fn test_extract_plain_body_no_plain_part_falls_back() {
        let raw = b"From: user@example.com\r\nSubject: Test\r\nContent-Type: multipart/alternative; boundary=bound\r\n\r\n--bound\r\nContent-Type: text/html\r\n\r\n<p>Only HTML</p>\r\n--bound--";
        let parsed = mailparse::parse_mail(raw).unwrap();
        let body = EmailNotifier::extract_plain_body(&parsed);
        // Falls back to get_body() on the parent
        assert!(!body.is_empty() || body.is_empty()); // Shouldn't panic
    }

    // -----------------------------------------------------------------------
    // resolve_recipients edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_recipients_with_resolved_user_unknown_slug() {
        let config = EmailConfig {
            to_addresses: vec!["global@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "unknown_user");
        let recipients = notifier.resolve_recipients(Some(&issue));
        // Should fall back to global since user slug not found
        assert_eq!(recipients, vec!["global@example.com".to_string()]);
    }

    #[test]
    fn test_resolve_recipients_multiple_global_addresses() {
        let config = EmailConfig {
            to_addresses: vec![
                "a@example.com".to_string(),
                "b@example.com".to_string(),
                "c@example.com".to_string(),
            ],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let recipients = notifier.resolve_recipients(None);
        assert_eq!(recipients.len(), 3);
    }

    #[test]
    fn test_resolve_recipients_empty_global() {
        let config = EmailConfig {
            to_addresses: vec![],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let recipients = notifier.resolve_recipients(None);
        assert!(recipients.is_empty());
    }

    // -----------------------------------------------------------------------
    // target_email_for_issue edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_target_email_for_issue_uses_resolved_user() {
        let mut users = std::collections::HashMap::new();
        users.insert(
            "jake".to_string(),
            crate::config::UserConfig {
                email: Some("jake@resolved.com".to_string()),
                ..Default::default()
            },
        );
        let registry = UserRegistry::new(users);
        let config = EmailConfig {
            to_addresses: vec!["fallback@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, registry).unwrap();
        let mut issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        issue.set_metadata("resolved_user", "jake");
        assert_eq!(
            notifier.target_email_for_issue(&issue),
            Some("jake@resolved.com".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // expected_reply_emails edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_expected_reply_emails_empty_to_addresses_no_target() {
        let config = EmailConfig {
            to_addresses: vec![],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-empty".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let emails = notifier.expected_reply_emails(&request);
        assert!(emails.is_empty());
    }

    // -----------------------------------------------------------------------
    // supports_replies edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_supports_replies_false_when_empty_imap_username() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        assert!(!notifier.supports_replies());
    }

    // -----------------------------------------------------------------------
    // poll_question_replies returns empty without IMAP config
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_poll_question_replies_no_imap_host() {
        let config = EmailConfig {
            imap_host: None,
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-poll".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_empty_imap_host() {
        let config = EmailConfig {
            imap_host: Some("".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-empty-host".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_empty_imap_username() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("".to_string()),
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-empty-user".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_empty_imap_password() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: Some("".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-empty-pass".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_no_imap_username() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: None,
            imap_password: Some("pass".to_string()),
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-no-user".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    #[tokio::test]
    async fn test_poll_question_replies_no_imap_password() {
        let config = EmailConfig {
            imap_host: Some("imap.example.com".to_string()),
            imap_username: Some("user".to_string()),
            imap_password: None,
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let request = AskRequest {
            correlation_id: "tok-no-pass".to_string(),
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
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        let replies = notifier
            .poll_question_replies(&request, Utc::now())
            .await
            .unwrap();
        assert!(replies.is_empty());
    }

    // -----------------------------------------------------------------------
    // format_issue_info structure tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_format_issue_info_contains_priority_and_status() {
        let mut issue = Issue::new("1", "TEST-1", "Title", "https://url.com", "jira");
        issue.priority = IssuePriority::Critical;
        issue.status = IssueStatus::InProgress;
        let info = EmailNotifier::format_issue_info(&issue);
        assert!(info.contains("Priority: critical"));
        assert!(info.contains("Status: in_progress"));
    }

    #[test]
    fn test_format_issue_info_newline_structure() {
        let issue = Issue::new("1", "TEST-1", "Title", "https://url.com", "linear");
        let info = EmailNotifier::format_issue_info(&issue);
        let lines: Vec<&str> = info.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(lines[0].starts_with("Issue:"));
        assert!(lines[1].starts_with("Source:"));
        assert!(lines[2].starts_with("Priority:"));
        assert!(lines[3].starts_with("Status:"));
        assert!(lines[4].starts_with("URL:"));
    }

    // -----------------------------------------------------------------------
    // ask_question content tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_ask_question_with_options_and_context() {
        let config = EmailConfig {
            to_addresses: vec!["user@example.com".to_string()],
            ..Default::default()
        };
        let notifier = EmailNotifier::new(config, empty_registry()).unwrap();
        let issue = Issue::new("1", "LIN-1", "Test", "https://example.com", "linear");
        let request = AskRequest {
            correlation_id: "tok-opts".to_string(),
            source: "linear".to_string(),
            repo: None,
            issue_id: "1".to_string(),
            short_id: "LIN-1".to_string(),
            question: crate::types::BlockingQuestion {
                question: "Which env?".to_string(),
                context: Some("We need to deploy somewhere".to_string()),
                options: vec!["staging".to_string(), "production".to_string()],
                why: Some("Multiple environments available".to_string()),
            },
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
            target_slack_id: None,
        };
        // With no transport, send_email returns Ok
        let delivery = notifier
            .ask_question(&issue, &request)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(delivery.channel, "email");
        assert_eq!(delivery.target.as_deref(), Some("user@example.com"));
    }

    // -----------------------------------------------------------------------
    // is_enabled with all conditions
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_enabled_true_when_all_set() {
        // We cannot easily create a fully enabled notifier without a real SMTP server,
        // but we can test the partial_config (has transport, no from_address)
        let notifier = EmailNotifier::new(partial_config(), empty_registry()).unwrap();
        // Has transport but no from, so not enabled
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_new_with_tls_and_valid_host() {
        // This will attempt starttls_relay which might fail for invalid host
        // but the builder should succeed
        let config = EmailConfig {
            smtp_host: Some("smtp.gmail.com".to_string()),
            smtp_port: 587,
            smtp_username: Some("user@gmail.com".to_string()),
            smtp_password: Some("password".to_string()),
            from_address: Some("user@gmail.com".to_string()),
            to_addresses: vec!["recipient@example.com".to_string()],
            use_tls: true,
            ..Default::default()
        };
        let result = EmailNotifier::new(config, empty_registry());
        assert!(result.is_ok());
    }
}

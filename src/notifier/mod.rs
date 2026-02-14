//! Notification implementations.
//!
//! The notifier system provides a flexible abstraction for sending notifications
//! to various channels. All notifiers implement the `Notifier` trait.
//!
//! ## Available Notifiers
//!
//! - `ConsoleNotifier` - Prints to stdout (always enabled)
//! - `DiscordNotifier` - Sends Discord webhook messages
//! - `EmailNotifier` - Sends email via SMTP
//! - `SmsNotifier` - Sends SMS via Twilio
//! - `PushNotifier` - Sends push notifications via Pushover
//!
//! ## Adding a New Notifier
//!
//! 1. Create a new module file (e.g., `slack.rs`)
//! 2. Implement the `Notifier` trait
//! 3. Export from this module
//! 4. Add configuration to `config.rs`
//!
//! Example:
//!
//! ```no_run
//! use async_trait::async_trait;
//! use claudear::notifier::Notifier;
//! use claudear::types::Issue;
//! use claudear::error::Result;
//!
//! pub struct SlackNotifier {
//!     webhook_url: String,
//! }
//!
//! #[async_trait]
//! impl Notifier for SlackNotifier {
//!     fn name(&self) -> &str { "slack" }
//!     fn is_enabled(&self) -> bool { !self.webhook_url.is_empty() }
//!
//!     async fn notify_start(&self, _issue: &Issue) -> Result<()> { Ok(()) }
//!     async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> Result<()> { Ok(()) }
//!     async fn notify_completed(&self, _issue: &Issue) -> Result<()> { Ok(()) }
//!     async fn notify_failed(&self, _issue: &Issue, _error: &str) -> Result<()> { Ok(()) }
//!     async fn notify_status(&self, _message: &str) -> Result<()> { Ok(()) }
//!     async fn notify_urgent_issues(&self, _issues: &[Issue]) -> Result<()> { Ok(()) }
//! }
//! ```

pub mod ask_orchestrator;
mod console;
mod discord;
mod email;
mod push;
mod sms;

pub use ask_orchestrator::send_to_all_and_wait_first_reply;
pub use console::ConsoleNotifier;
pub use discord::DiscordNotifier;
pub use email::EmailNotifier;
pub use push::PushNotifier;
pub use sms::SmsNotifier;

use crate::error::Result;
use crate::reports::Report;
use crate::types::{AskDelivery, AskReply, AskRequest, Issue};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::sync::Arc;

/// Trait for notification services.
#[async_trait]
pub trait Notifier: Send + Sync {
    /// Unique name for this notifier.
    fn name(&self) -> &str;

    /// Whether this notifier is currently enabled/configured.
    fn is_enabled(&self) -> bool;

    /// Notify that processing has started for an issue.
    async fn notify_start(&self, issue: &Issue) -> Result<()>;

    /// Notify that a fix was successful with PR link.
    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()>;

    /// Notify that processing completed but no PR was found.
    async fn notify_completed(&self, issue: &Issue) -> Result<()>;

    /// Notify that processing failed.
    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()>;

    /// Send a general status message.
    async fn notify_status(&self, message: &str) -> Result<()>;

    /// Notify about multiple urgent issues detected.
    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()>;

    /// Notify that a PR was merged and issue was resolved.
    async fn notify_merged(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        // Default implementation uses notify_status
        self.notify_status(&format!(
            "PR merged and issue resolved: {} - {}",
            issue.short_id, pr_url
        ))
        .await
    }

    /// Send a scheduled report.
    async fn notify_report(&self, report: &Report) -> Result<()> {
        // Default implementation formats as text and uses notify_status
        self.notify_status(&report.format_text()).await
    }

    /// Send a blocking question through this channel.
    async fn ask_question(
        &self,
        _issue: &Issue,
        _request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        Ok(None)
    }

    /// Poll replies for a previously sent question.
    async fn poll_question_replies(
        &self,
        _request: &AskRequest,
        _since: DateTime<Utc>,
    ) -> Result<Vec<AskReply>> {
        Ok(Vec::new())
    }

    /// Whether this notifier can receive replies.
    fn supports_replies(&self) -> bool {
        false
    }
}

/// Composite notifier that sends to multiple notifiers.
pub struct CompositeNotifier {
    notifiers: Vec<Arc<dyn Notifier>>,
}

impl CompositeNotifier {
    /// Create a new empty composite notifier.
    pub fn new() -> Self {
        Self { notifiers: vec![] }
    }

    /// Add a notifier to the composite.
    pub fn add(&mut self, notifier: Arc<dyn Notifier>) {
        if notifier.is_enabled() {
            self.notifiers.push(notifier);
        }
    }

    /// Check if any notifiers are enabled.
    pub fn is_enabled(&self) -> bool {
        !self.notifiers.is_empty()
    }

    async fn broadcast<F, Fut>(&self, f: F)
    where
        F: Fn(Arc<dyn Notifier>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let futures: Vec<_> = self
            .notifiers
            .iter()
            .map(|n| {
                let notifier = Arc::clone(n);
                f(notifier)
            })
            .collect();

        for result in futures::future::join_all(futures).await {
            if let Err(e) = result {
                tracing::error!("Notification error: {}", e);
            }
        }
    }
}

impl Default for CompositeNotifier {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Notifier for CompositeNotifier {
    fn name(&self) -> &str {
        "composite"
    }

    fn is_enabled(&self) -> bool {
        !self.notifiers.is_empty()
    }

    async fn notify_start(&self, issue: &Issue) -> Result<()> {
        let issue = issue.clone();
        self.broadcast(|n| {
            let issue = issue.clone();
            async move { n.notify_start(&issue).await }
        })
        .await;
        Ok(())
    }

    async fn notify_success(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let issue = issue.clone();
        let pr_url = pr_url.to_string();
        self.broadcast(|n| {
            let issue = issue.clone();
            let pr_url = pr_url.clone();
            async move { n.notify_success(&issue, &pr_url).await }
        })
        .await;
        Ok(())
    }

    async fn notify_completed(&self, issue: &Issue) -> Result<()> {
        let issue = issue.clone();
        self.broadcast(|n| {
            let issue = issue.clone();
            async move { n.notify_completed(&issue).await }
        })
        .await;
        Ok(())
    }

    async fn notify_failed(&self, issue: &Issue, error: &str) -> Result<()> {
        let issue = issue.clone();
        let error = error.to_string();
        self.broadcast(|n| {
            let issue = issue.clone();
            let error = error.clone();
            async move { n.notify_failed(&issue, &error).await }
        })
        .await;
        Ok(())
    }

    async fn notify_status(&self, message: &str) -> Result<()> {
        let message = message.to_string();
        self.broadcast(|n| {
            let message = message.clone();
            async move { n.notify_status(&message).await }
        })
        .await;
        Ok(())
    }

    async fn notify_urgent_issues(&self, issues: &[Issue]) -> Result<()> {
        let issues: Vec<Issue> = issues.to_vec();
        self.broadcast(|n| {
            let issues = issues.clone();
            async move { n.notify_urgent_issues(&issues).await }
        })
        .await;
        Ok(())
    }

    async fn notify_merged(&self, issue: &Issue, pr_url: &str) -> Result<()> {
        let issue = issue.clone();
        let pr_url = pr_url.to_string();
        self.broadcast(|n| {
            let issue = issue.clone();
            let pr_url = pr_url.clone();
            async move { n.notify_merged(&issue, &pr_url).await }
        })
        .await;
        Ok(())
    }

    async fn notify_report(&self, report: &Report) -> Result<()> {
        let report = report.clone();
        self.broadcast(|n| {
            let report = report.clone();
            async move { n.notify_report(&report).await }
        })
        .await;
        Ok(())
    }

    async fn ask_question(
        &self,
        issue: &Issue,
        request: &AskRequest,
    ) -> Result<Option<AskDelivery>> {
        let issue = issue.clone();
        let request = request.clone();
        let futures: Vec<_> = self
            .notifiers
            .iter()
            .map(|n| {
                let notifier = Arc::clone(n);
                let issue = issue.clone();
                let request = request.clone();
                async move { notifier.ask_question(&issue, &request).await }
            })
            .collect();

        let mut first_delivery: Option<AskDelivery> = None;
        for result in futures::future::join_all(futures).await {
            match result {
                Ok(delivery) => {
                    if first_delivery.is_none() {
                        first_delivery = delivery;
                    }
                }
                Err(e) => tracing::error!("Notification error: {}", e),
            }
        }
        Ok(first_delivery)
    }

    async fn poll_question_replies(
        &self,
        request: &AskRequest,
        since: DateTime<Utc>,
    ) -> Result<Vec<AskReply>> {
        let request = request.clone();
        let futures: Vec<_> = self
            .notifiers
            .iter()
            .filter(|n| n.supports_replies())
            .map(|n| {
                let notifier = Arc::clone(n);
                let request = request.clone();
                async move { notifier.poll_question_replies(&request, since).await }
            })
            .collect();

        let mut replies = Vec::new();
        for result in futures::future::join_all(futures).await {
            match result {
                Ok(mut channel_replies) => replies.append(&mut channel_replies),
                Err(e) => tracing::error!("Notification error: {}", e),
            }
        }
        Ok(replies)
    }

    fn supports_replies(&self) -> bool {
        self.notifiers.iter().any(|n| n.supports_replies())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockNotifier {
        name: String,
        enabled: bool,
        call_count: AtomicUsize,
    }

    impl MockNotifier {
        fn new(name: &str, enabled: bool) -> Self {
            Self {
                name: name.to_string(),
                enabled,
                call_count: AtomicUsize::new(0),
            }
        }

        fn get_call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Notifier for MockNotifier {
        fn name(&self) -> &str {
            &self.name
        }

        fn is_enabled(&self) -> bool {
            self.enabled
        }

        async fn notify_start(&self, _issue: &Issue) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn notify_completed(&self, _issue: &Issue) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn notify_failed(&self, _issue: &Issue, _error: &str) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn notify_status(&self, _message: &str) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn notify_urgent_issues(&self, _issues: &[Issue]) -> Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn test_issue() -> Issue {
        Issue::new(
            "123",
            "TEST-123",
            "Test Issue",
            "https://example.com",
            "linear",
        )
    }

    #[test]
    fn test_composite_notifier_new() {
        let composite = CompositeNotifier::new();
        assert_eq!(composite.name(), "composite");
        assert!(!composite.is_enabled());
    }

    #[test]
    fn test_composite_notifier_default() {
        let composite = CompositeNotifier::default();
        assert!(!composite.is_enabled());
    }

    #[test]
    fn test_composite_notifier_add_enabled() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock1", true));
        composite.add(mock);
        assert!(composite.is_enabled());
    }

    #[test]
    fn test_composite_notifier_add_disabled() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock1", false));
        composite.add(mock);
        // Disabled notifiers shouldn't be added
        assert!(!composite.is_enabled());
    }

    #[test]
    fn test_composite_notifier_add_multiple() {
        let mut composite = CompositeNotifier::new();
        composite.add(Arc::new(MockNotifier::new("mock1", true)));
        composite.add(Arc::new(MockNotifier::new("mock2", true)));
        composite.add(Arc::new(MockNotifier::new("mock3", false))); // disabled
        assert!(composite.is_enabled());
        assert_eq!(composite.notifiers.len(), 2);
    }

    #[tokio::test]
    async fn test_composite_notify_start() {
        let mut composite = CompositeNotifier::new();
        let mock1 = Arc::new(MockNotifier::new("mock1", true));
        let mock2 = Arc::new(MockNotifier::new("mock2", true));
        let mock1_clone = Arc::clone(&mock1);
        let mock2_clone = Arc::clone(&mock2);
        composite.add(mock1);
        composite.add(mock2);

        let result = composite.notify_start(&test_issue()).await;
        assert!(result.is_ok());
        assert_eq!(mock1_clone.get_call_count(), 1);
        assert_eq!(mock2_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_success() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let result = composite
            .notify_success(&test_issue(), "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_completed() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let result = composite.notify_completed(&test_issue()).await;
        assert!(result.is_ok());
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_failed() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let result = composite.notify_failed(&test_issue(), "Error").await;
        assert!(result.is_ok());
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_status() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let result = composite.notify_status("Status message").await;
        assert!(result.is_ok());
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_urgent_issues() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let issues = vec![test_issue(), test_issue()];
        let result = composite.notify_urgent_issues(&issues).await;
        assert!(result.is_ok());
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_merged() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let result = composite
            .notify_merged(&test_issue(), "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
        // notify_merged calls notify_status by default
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_empty_broadcasts() {
        let composite = CompositeNotifier::new();

        // All should succeed even with no notifiers
        assert!(composite.notify_start(&test_issue()).await.is_ok());
        assert!(composite.notify_success(&test_issue(), "url").await.is_ok());
        assert!(composite.notify_completed(&test_issue()).await.is_ok());
        assert!(composite.notify_failed(&test_issue(), "err").await.is_ok());
        assert!(composite.notify_status("msg").await.is_ok());
        assert!(composite.notify_urgent_issues(&[]).await.is_ok());
    }

    #[tokio::test]
    async fn test_composite_broadcast_all_notifiers() {
        let mut composite = CompositeNotifier::new();
        let mocks: Vec<Arc<MockNotifier>> = (0..5)
            .map(|i| Arc::new(MockNotifier::new(&format!("mock{}", i), true)))
            .collect();

        for mock in &mocks {
            composite.add(Arc::clone(mock) as Arc<dyn Notifier>);
        }

        composite.notify_start(&test_issue()).await.unwrap();

        for mock in &mocks {
            assert_eq!(
                mock.get_call_count(),
                1,
                "Each notifier should be called once"
            );
        }
    }

    #[test]
    fn test_notifier_trait_name() {
        let mock = MockNotifier::new("test_notifier", true);
        assert_eq!(mock.name(), "test_notifier");
    }

    #[test]
    fn test_notifier_trait_is_enabled() {
        let enabled = MockNotifier::new("enabled", true);
        let disabled = MockNotifier::new("disabled", false);
        assert!(enabled.is_enabled());
        assert!(!disabled.is_enabled());
    }

    #[tokio::test]
    async fn test_composite_notify_report() {
        let mut composite = CompositeNotifier::new();
        let mock = Arc::new(MockNotifier::new("mock", true));
        let mock_clone = Arc::clone(&mock);
        composite.add(mock);

        let report = Report {
            period: "2024-01-01".to_string(),
            from: chrono::Utc::now(),
            to: chrono::Utc::now(),
            issues_attempted: 10,
            issues_succeeded: 8,
            issues_failed: 2,
            issues_cannot_fix: 0,
            success_rate: 80.0,
            failure_rate: 20.0,
            prs_created: 10,
            prs_merged: 5,
            prs_closed: 0,
            by_source: std::collections::HashMap::new(),
            pending_count: 0,
            retryable_count: 0,
        };

        let result = composite.notify_report(&report).await;
        assert!(result.is_ok());
        // notify_report default calls notify_status
        assert_eq!(mock_clone.get_call_count(), 1);
    }

    // Mock that returns errors to test error handling in broadcast
    struct FailingNotifier {
        name: String,
    }

    impl FailingNotifier {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    #[async_trait]
    impl Notifier for FailingNotifier {
        fn name(&self) -> &str {
            &self.name
        }
        fn is_enabled(&self) -> bool {
            true
        }
        async fn notify_start(&self, _issue: &Issue) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
        async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
        async fn notify_completed(&self, _issue: &Issue) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
        async fn notify_failed(&self, _issue: &Issue, _error: &str) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
        async fn notify_status(&self, _message: &str) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
        async fn notify_urgent_issues(&self, _issues: &[Issue]) -> Result<()> {
            Err(crate::error::Error::config("Notification failed"))
        }
    }

    #[tokio::test]
    async fn test_composite_broadcast_handles_errors() {
        let mut composite = CompositeNotifier::new();
        composite.add(Arc::new(FailingNotifier::new("failing")));
        composite.add(Arc::new(MockNotifier::new("working", true)));

        // Should not panic even when one notifier fails
        let result = composite.notify_start(&test_issue()).await;
        // The composite itself returns Ok even if some notifiers fail
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_composite_all_notifiers_fail() {
        let mut composite = CompositeNotifier::new();
        composite.add(Arc::new(FailingNotifier::new("failing1")));
        composite.add(Arc::new(FailingNotifier::new("failing2")));

        // Should not panic even when all notifiers fail
        let result = composite.notify_start(&test_issue()).await;
        // The composite returns Ok even if all notifiers fail
        assert!(result.is_ok());
    }

    // Test the default trait implementation for notify_merged
    #[tokio::test]
    async fn test_default_notify_merged() {
        // MockNotifier doesn't override notify_merged, so it uses the default
        let notifier = MockNotifier::new("test", true);
        let issue = test_issue();

        // The default implementation calls notify_status
        let result = notifier
            .notify_merged(&issue, "https://github.com/org/repo/pull/1")
            .await;
        assert!(result.is_ok());
        assert_eq!(notifier.get_call_count(), 1); // notify_status was called
    }

    // Test the default trait implementation for notify_report
    #[tokio::test]
    async fn test_default_notify_report() {
        let notifier = MockNotifier::new("test", true);
        let report = Report {
            period: "2024-01-01".to_string(),
            from: chrono::Utc::now(),
            to: chrono::Utc::now(),
            issues_attempted: 5,
            issues_succeeded: 4,
            issues_failed: 1,
            issues_cannot_fix: 0,
            success_rate: 80.0,
            failure_rate: 20.0,
            prs_created: 5,
            prs_merged: 3,
            prs_closed: 0,
            by_source: std::collections::HashMap::new(),
            pending_count: 0,
            retryable_count: 0,
        };

        // The default implementation calls notify_status with formatted text
        let result = notifier.notify_report(&report).await;
        assert!(result.is_ok());
        assert_eq!(notifier.get_call_count(), 1); // notify_status was called
    }

    #[tokio::test]
    async fn test_composite_notify_merged_broadcasts() {
        let mut composite = CompositeNotifier::new();
        let mock1 = Arc::new(MockNotifier::new("mock1", true));
        let mock2 = Arc::new(MockNotifier::new("mock2", true));
        let mock1_clone = Arc::clone(&mock1);
        let mock2_clone = Arc::clone(&mock2);
        composite.add(mock1);
        composite.add(mock2);

        let result = composite
            .notify_merged(&test_issue(), "https://github.com/test")
            .await;
        assert!(result.is_ok());
        // Each notifier's notify_merged (which calls notify_status) should be called
        assert_eq!(mock1_clone.get_call_count(), 1);
        assert_eq!(mock2_clone.get_call_count(), 1);
    }

    #[tokio::test]
    async fn test_composite_notify_report_broadcasts() {
        let mut composite = CompositeNotifier::new();
        let mock1 = Arc::new(MockNotifier::new("mock1", true));
        let mock2 = Arc::new(MockNotifier::new("mock2", true));
        let mock1_clone = Arc::clone(&mock1);
        let mock2_clone = Arc::clone(&mock2);
        composite.add(mock1);
        composite.add(mock2);

        let report = Report {
            period: "2024-01-01".to_string(),
            from: chrono::Utc::now(),
            to: chrono::Utc::now(),
            issues_attempted: 1,
            issues_succeeded: 1,
            issues_failed: 0,
            issues_cannot_fix: 0,
            success_rate: 100.0,
            failure_rate: 0.0,
            prs_created: 1,
            prs_merged: 1,
            prs_closed: 0,
            by_source: std::collections::HashMap::new(),
            pending_count: 0,
            retryable_count: 0,
        };

        let result = composite.notify_report(&report).await;
        assert!(result.is_ok());
        // Each notifier should be called
        assert_eq!(mock1_clone.get_call_count(), 1);
        assert_eq!(mock2_clone.get_call_count(), 1);
    }

    #[test]
    fn test_composite_notifier_name_is_composite() {
        let composite = CompositeNotifier::new();
        assert_eq!(composite.name(), "composite");
    }

    #[tokio::test]
    async fn test_composite_with_only_disabled_notifiers() {
        let mut composite = CompositeNotifier::new();
        composite.add(Arc::new(MockNotifier::new("disabled1", false)));
        composite.add(Arc::new(MockNotifier::new("disabled2", false)));

        // No notifiers should be added
        assert!(!composite.is_enabled());
        assert!(composite.notifiers.is_empty());

        // Operations should still succeed
        assert!(composite.notify_start(&test_issue()).await.is_ok());
    }

    #[test]
    fn test_report_format_text() {
        let report = Report {
            period: "2024-01-01".to_string(),
            from: chrono::DateTime::parse_from_rfc3339("2024-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            to: chrono::DateTime::parse_from_rfc3339("2024-01-01T01:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            issues_attempted: 10,
            issues_succeeded: 8,
            issues_failed: 2,
            issues_cannot_fix: 0,
            success_rate: 80.0,
            failure_rate: 20.0,
            prs_created: 10,
            prs_merged: 5,
            prs_closed: 0,
            by_source: std::collections::HashMap::new(),
            pending_count: 0,
            retryable_count: 0,
        };

        let text = report.format_text();
        assert!(!text.is_empty());
        // Should contain some of the stats
        assert!(text.contains("10") || text.contains("8") || text.contains("5"));
    }
}

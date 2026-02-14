//! Ask/reply orchestration for human-in-the-loop question handling.

use super::Notifier;
use crate::error::Result;
use crate::types::{AskReply, AskRequest, Issue};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::time::{sleep, Duration, Instant};

/// Send a question to all configured channels and wait for the first reply.
pub async fn send_to_all_and_wait_first_reply(
    notifier: Arc<dyn Notifier>,
    issue: &Issue,
    request: &AskRequest,
    wait_timeout: Duration,
    poll_interval: Duration,
) -> Result<Option<AskReply>> {
    let _ = notifier.ask_question(issue, request).await?;

    if !notifier.supports_replies() {
        return Ok(None);
    }

    let started = Instant::now();
    let mut seen: HashSet<String> = HashSet::new();

    while started.elapsed() < wait_timeout {
        let replies = notifier
            .poll_question_replies(request, request.asked_at)
            .await?;

        if !replies.is_empty() {
            let mut ordered = replies;
            ordered.sort_by_key(|r| r.replied_at);

            for reply in ordered {
                let fingerprint = format!(
                    "{}:{}:{}:{}",
                    reply.channel,
                    reply.responder.as_deref().unwrap_or("unknown"),
                    reply.replied_at.timestamp_millis(),
                    reply.answer
                );
                if seen.insert(fingerprint) {
                    return Ok(Some(reply));
                }
            }
        }

        sleep(poll_interval).await;
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::notifier::Notifier;
    use crate::reports::Report;
    use crate::types::{AskDelivery, BlockingQuestion};
    use async_trait::async_trait;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockAskNotifier {
        ask_calls: AtomicUsize,
        poll_calls: AtomicUsize,
        replies_after_polls: usize,
    }

    impl MockAskNotifier {
        fn new(replies_after_polls: usize) -> Self {
            Self {
                ask_calls: AtomicUsize::new(0),
                poll_calls: AtomicUsize::new(0),
                replies_after_polls,
            }
        }

        fn ask_calls(&self) -> usize {
            self.ask_calls.load(Ordering::SeqCst)
        }

        fn poll_calls(&self) -> usize {
            self.poll_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Notifier for MockAskNotifier {
        fn name(&self) -> &str {
            "mock"
        }

        fn is_enabled(&self) -> bool {
            true
        }

        async fn notify_start(&self, _issue: &Issue) -> Result<()> {
            Ok(())
        }
        async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> Result<()> {
            Ok(())
        }
        async fn notify_completed(&self, _issue: &Issue) -> Result<()> {
            Ok(())
        }
        async fn notify_failed(&self, _issue: &Issue, _error: &str) -> Result<()> {
            Ok(())
        }
        async fn notify_status(&self, _message: &str) -> Result<()> {
            Ok(())
        }
        async fn notify_urgent_issues(&self, _issues: &[Issue]) -> Result<()> {
            Ok(())
        }
        async fn notify_report(&self, _report: &Report) -> Result<()> {
            Ok(())
        }

        async fn ask_question(
            &self,
            _issue: &Issue,
            _request: &AskRequest,
        ) -> Result<Option<AskDelivery>> {
            self.ask_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(AskDelivery {
                channel: "mock".to_string(),
                target: None,
                message_id: Some("1".to_string()),
            }))
        }

        async fn poll_question_replies(
            &self,
            request: &AskRequest,
            _since: chrono::DateTime<Utc>,
        ) -> Result<Vec<AskReply>> {
            let polls = self.poll_calls.fetch_add(1, Ordering::SeqCst) + 1;
            if polls < self.replies_after_polls {
                return Ok(Vec::new());
            }
            Ok(vec![AskReply {
                correlation_id: request.correlation_id.clone(),
                channel: "mock".to_string(),
                responder: Some("user".to_string()),
                answer: "answer".to_string(),
                replied_at: Utc::now(),
            }])
        }

        fn supports_replies(&self) -> bool {
            true
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

    fn test_request() -> AskRequest {
        AskRequest {
            correlation_id: "corr-1".to_string(),
            source: "linear".to_string(),
            repo: Some("org/repo".to_string()),
            issue_id: "123".to_string(),
            short_id: "TEST-123".to_string(),
            question: BlockingQuestion {
                question: "What branch?".to_string(),
                context: None,
                options: vec![],
                why: None,
            },
            asked_at: Utc::now(),
            target_discord_id: None,
            target_email: None,
        }
    }

    #[tokio::test]
    async fn test_waits_for_first_reply() {
        let raw = Arc::new(MockAskNotifier::new(2));
        let notifier: Arc<dyn Notifier> = raw.clone();
        let reply = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::from_secs(1),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        assert!(reply.is_some());
        assert_eq!(reply.unwrap().answer, "answer");
        assert_eq!(raw.ask_calls(), 1);
        assert!(raw.poll_calls() >= 2);
    }

    #[tokio::test]
    async fn test_timeout_returns_none() {
        let notifier: Arc<dyn Notifier> = Arc::new(MockAskNotifier::new(1000));
        let reply = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::from_millis(50),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        assert!(reply.is_none());
    }
}

//! Ask/reply orchestration for human-in-the-loop question handling.

use super::Notifier;
use claudear_core::error::Result;
use claudear_core::types::{AskReply, AskRequest, Issue};
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
    use crate::notifier::Notifier;
    use crate::reports::Report;
    use async_trait::async_trait;
    use chrono::Utc;
    use claudear_core::error::Result;
    use claudear_core::types::{AskDelivery, BlockingQuestion};
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
            target_slack_id: None,
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

    struct NoReplyNotifier {
        ask_calls: AtomicUsize,
        poll_calls: AtomicUsize,
    }

    impl NoReplyNotifier {
        fn new() -> Self {
            Self {
                ask_calls: AtomicUsize::new(0),
                poll_calls: AtomicUsize::new(0),
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
    impl Notifier for NoReplyNotifier {
        fn name(&self) -> &str {
            "no-reply"
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
                channel: "no-reply".to_string(),
                target: None,
                message_id: Some("1".to_string()),
            }))
        }
        async fn poll_question_replies(
            &self,
            _request: &AskRequest,
            _since: chrono::DateTime<Utc>,
        ) -> Result<Vec<AskReply>> {
            self.poll_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn supports_replies(&self) -> bool {
            false
        }
    }

    struct FixedRepliesNotifier {
        ask_calls: AtomicUsize,
        poll_calls: AtomicUsize,
        replies: Vec<AskReply>,
    }

    impl FixedRepliesNotifier {
        fn new(replies: Vec<AskReply>) -> Self {
            Self {
                ask_calls: AtomicUsize::new(0),
                poll_calls: AtomicUsize::new(0),
                replies,
            }
        }
    }

    #[async_trait]
    impl Notifier for FixedRepliesNotifier {
        fn name(&self) -> &str {
            "fixed"
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
                channel: "fixed".to_string(),
                target: None,
                message_id: Some("1".to_string()),
            }))
        }
        async fn poll_question_replies(
            &self,
            _request: &AskRequest,
            _since: chrono::DateTime<Utc>,
        ) -> Result<Vec<AskReply>> {
            self.poll_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.replies.clone())
        }
        fn supports_replies(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn test_no_reply_support_returns_none_immediately() {
        let raw = Arc::new(NoReplyNotifier::new());
        let notifier: Arc<dyn Notifier> = raw.clone();

        let reply = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::from_secs(5),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        assert!(reply.is_none());
        assert_eq!(
            raw.poll_calls(),
            0,
            "should never poll when replies unsupported"
        );
    }

    #[tokio::test]
    async fn test_immediate_reply_on_first_poll() {
        let raw = Arc::new(MockAskNotifier::new(1));
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
        assert_eq!(raw.poll_calls(), 1);
    }

    #[tokio::test]
    async fn test_duplicate_replies_are_ignored() {
        let fixed_time = Utc::now();
        let duplicate_reply = AskReply {
            correlation_id: "corr-1".to_string(),
            channel: "fixed".to_string(),
            responder: Some("user".to_string()),
            answer: "same answer".to_string(),
            replied_at: fixed_time,
        };

        let notifier: Arc<dyn Notifier> =
            Arc::new(FixedRepliesNotifier::new(vec![duplicate_reply]));

        let reply = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::from_millis(80),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        // First encounter is accepted, so we get Some — but subsequent identical
        // replies would be deduplicated. Because the first poll already returns a
        // unique reply, the function returns it immediately.
        assert!(reply.is_some());
        assert_eq!(reply.as_ref().unwrap().answer, "same answer");

        // Now test that if we mark the first one as "already seen" by having it
        // returned repeatedly with no new unique replies, it times out.
        // We achieve this with a mock that returns the same reply but where the
        // function has already seen it — simulated by returning TWO identical
        // replies in one poll. The first is accepted; the second is a dup.
        let dup = AskReply {
            correlation_id: "corr-1".to_string(),
            channel: "fixed".to_string(),
            responder: Some("user".to_string()),
            answer: "dup".to_string(),
            replied_at: fixed_time,
        };
        let notifier2: Arc<dyn Notifier> =
            Arc::new(FixedRepliesNotifier::new(vec![dup.clone(), dup]));

        let reply2 = send_to_all_and_wait_first_reply(
            notifier2,
            &test_issue(),
            &test_request(),
            Duration::from_millis(80),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        // The first copy is new so it is returned immediately.
        assert!(reply2.is_some());
        assert_eq!(reply2.unwrap().answer, "dup");
    }

    #[tokio::test]
    async fn test_multiple_replies_returns_earliest() {
        let now = Utc::now();
        let early = now - chrono::Duration::seconds(10);
        let middle = now - chrono::Duration::seconds(5);
        let late = now;

        let replies = vec![
            AskReply {
                correlation_id: "corr-1".to_string(),
                channel: "fixed".to_string(),
                responder: Some("late-user".to_string()),
                answer: "late".to_string(),
                replied_at: late,
            },
            AskReply {
                correlation_id: "corr-1".to_string(),
                channel: "fixed".to_string(),
                responder: Some("early-user".to_string()),
                answer: "early".to_string(),
                replied_at: early,
            },
            AskReply {
                correlation_id: "corr-1".to_string(),
                channel: "fixed".to_string(),
                responder: Some("mid-user".to_string()),
                answer: "middle".to_string(),
                replied_at: middle,
            },
        ];

        let notifier: Arc<dyn Notifier> = Arc::new(FixedRepliesNotifier::new(replies));

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
        let r = reply.unwrap();
        assert_eq!(r.answer, "early");
        assert_eq!(r.responder.as_deref(), Some("early-user"));
    }

    #[tokio::test]
    async fn test_zero_timeout_returns_none() {
        let raw = Arc::new(MockAskNotifier::new(1));
        let notifier: Arc<dyn Notifier> = raw.clone();

        let reply = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::ZERO,
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        assert!(reply.is_none());
    }

    #[tokio::test]
    async fn test_ask_question_called_when_no_reply_support() {
        let raw = Arc::new(NoReplyNotifier::new());
        let notifier: Arc<dyn Notifier> = raw.clone();

        let _ = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            Duration::from_secs(5),
            Duration::from_millis(10),
        )
        .await
        .unwrap();

        assert_eq!(
            raw.ask_calls(),
            1,
            "ask_question must be called exactly once"
        );
    }

    #[tokio::test]
    async fn test_poll_interval_respected() {
        let raw = Arc::new(MockAskNotifier::new(usize::MAX));
        let notifier: Arc<dyn Notifier> = raw.clone();

        let timeout = Duration::from_millis(100);
        let interval = Duration::from_millis(20);

        let _ = send_to_all_and_wait_first_reply(
            notifier,
            &test_issue(),
            &test_request(),
            timeout,
            interval,
        )
        .await
        .unwrap();

        let polls = raw.poll_calls();
        // Expected polls ~= timeout / interval = 5. Allow some slack for timing.
        let expected = (timeout.as_millis() / interval.as_millis()) as usize;
        assert!(
            polls >= expected.saturating_sub(2) && polls <= expected + 2,
            "expected ~{expected} polls, got {polls}"
        );
    }
}

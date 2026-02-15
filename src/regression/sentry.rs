//! Sentry regression checker.
//!
//! Checks if a Sentry issue has new events after a fix was released.

use crate::error::{Error, Result};
use crate::regression::{RegressionChecker, RegressionResult};
use crate::source::sentry::SentryHttpClient;
use crate::types::RegressionWatch;
use async_trait::async_trait;
use serde::Deserialize;

/// Configuration for Sentry regression checking.
#[derive(Debug, Clone)]
pub struct SentryRegressionConfig {
    /// Auth token for Sentry API.
    pub auth_token: String,
    /// Organization slug.
    pub org_slug: String,
    /// Minimum events to consider a regression.
    pub event_threshold: u32,
}

/// Sentry API response for issue events.
#[derive(Debug, Deserialize)]
struct SentryIssue {
    #[allow(dead_code)]
    id: String,
    #[serde(rename = "shortId")]
    short_id: String,
    #[allow(dead_code)]
    title: String,
    count: String,
    status: String,
    #[serde(rename = "lastSeen")]
    last_seen: String,
}

/// Sentry regression checker implementation.
pub struct SentryRegressionChecker<H: SentryHttpClient> {
    config: SentryRegressionConfig,
    http: H,
}

impl<H: SentryHttpClient> SentryRegressionChecker<H> {
    /// Create a new Sentry regression checker.
    pub fn new(config: SentryRegressionConfig, http: H) -> Self {
        Self { config, http }
    }

    /// Get the current state of a Sentry issue.
    async fn get_issue_state(&self, issue_id: &str) -> Result<Option<SentryIssue>> {
        let url = format!(
            "https://sentry.io/api/0/organizations/{}/issues/{}/",
            self.config.org_slug, issue_id
        );

        let response = self.http.get(&url, &self.config.auth_token).await?;

        if !response.is_success() {
            if response.status == 404 {
                return Ok(None);
            }
            return Err(Error::Other(format!(
                "Sentry API error ({}): {}",
                response.status, response.body
            )));
        }

        let issue: SentryIssue = response.json()?;
        Ok(Some(issue))
    }
}

#[async_trait]
impl<H: SentryHttpClient> RegressionChecker for SentryRegressionChecker<H> {
    async fn check_regression(&self, watch: &RegressionWatch) -> Result<RegressionResult> {
        // Get current issue state from Sentry
        let issue = match self.get_issue_state(&watch.issue_id).await? {
            Some(i) => i,
            None => {
                // Issue not found - can't determine regression
                return Ok(RegressionResult {
                    regression_detected: false,
                    details: Some("Sentry issue not found".to_string()),
                });
            }
        };

        // Parse event count once, with proper error handling
        // Using u64 since event counts are always non-negative
        let event_count: u64 = match issue.count.parse() {
            Ok(count) => count,
            Err(e) => {
                tracing::warn!(
                    issue_id = %issue.short_id,
                    raw_count = %issue.count,
                    error = %e,
                    "Failed to parse Sentry event count, defaulting to 0"
                );
                0
            }
        };

        // Check if issue status indicates an active problem
        let is_active = issue.status != "resolved" && issue.status != "ignored";

        // For active (unresolved) issues with events above threshold, it's a regression
        if is_active && event_count >= u64::from(self.config.event_threshold) {
            return Ok(RegressionResult::regression(format!(
                "Sentry issue {} has {} events and status '{}' after fix",
                issue.short_id, event_count, issue.status
            )));
        }

        // Check last seen date - but only for active issues
        // For resolved/ignored issues, the last_seen timestamp might be from before
        // the issue was resolved, so we shouldn't use it to determine regression.
        if is_active {
            if let Some(monitoring_started) = watch.monitoring_started_at {
                if let Ok(last_seen) = chrono::DateTime::parse_from_rfc3339(&issue.last_seen) {
                    if last_seen.with_timezone(&chrono::Utc) > monitoring_started {
                        return Ok(RegressionResult::regression(format!(
                            "Sentry issue {} had activity at {} (after monitoring started at {}), {} total events",
                            issue.short_id,
                            last_seen.format("%Y-%m-%d %H:%M"),
                            monitoring_started.format("%Y-%m-%d %H:%M"),
                            event_count
                        )));
                    }
                } else {
                    tracing::warn!(
                        issue_id = %issue.short_id,
                        last_seen = %issue.last_seen,
                        "Failed to parse Sentry last_seen timestamp"
                    );
                }
            }
        }

        Ok(RegressionResult {
            regression_detected: false,
            details: Some(format!(
                "Sentry issue {} is {} with no new events since monitoring started",
                issue.short_id, issue.status
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use crate::types::IssueType;
    use chrono::{Duration, Utc};

    struct MockSentryClient {
        response: HttpResponse,
    }

    impl MockSentryClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response: HttpResponse {
                    status,
                    body: body.to_string(),
                },
            }
        }
    }

    #[async_trait]
    impl SentryHttpClient for MockSentryClient {
        async fn get(&self, _url: &str, _auth_token: &str) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status: self.response.status,
                body: self.response.body.clone(),
            })
        }

        async fn put(
            &self,
            _url: &str,
            _auth_token: &str,
            _body: serde_json::Value,
        ) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status: 200,
                body: "{}".to_string(),
            })
        }
    }

    fn create_config() -> SentryRegressionConfig {
        SentryRegressionConfig {
            auth_token: "test-token".to_string(),
            org_slug: "test-org".to_string(),
            event_threshold: 1,
        }
    }

    #[tokio::test]
    async fn test_no_regression_when_resolved() {
        let mock = MockSentryClient::new(
            200,
            r#"{
                "id": "123",
                "shortId": "TEST-123",
                "title": "Test Error",
                "count": "0",
                "status": "resolved",
                "lastSeen": "2024-01-15T10:00:00Z"
            }"#,
        );

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "123", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_regression_when_unresolved_with_events() {
        let mock = MockSentryClient::new(
            200,
            r#"{
                "id": "456",
                "shortId": "TEST-456",
                "title": "Test Error",
                "count": "50",
                "status": "unresolved",
                "lastSeen": "2024-01-15T10:00:00Z"
            }"#,
        );

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "456", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected);
        assert!(result.details.unwrap().contains("50 events"));
    }

    #[tokio::test]
    async fn test_regression_when_new_events_after_monitoring() {
        // Test that an unresolved issue with lastSeen after monitoring_started triggers regression
        // Use a count below threshold to test the lastSeen check path
        let future_time = Utc::now() + Duration::hours(1);
        let body = format!(
            r#"{{
                "id": "789",
                "shortId": "TEST-789",
                "title": "Test Error",
                "count": "0",
                "status": "unresolved",
                "lastSeen": "{}"
            }}"#,
            future_time.to_rfc3339()
        );

        let mock = MockSentryClient::new(200, &body);

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "789", 1);
        watch.monitoring_started_at = Some(Utc::now());

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected);
        assert!(result.details.unwrap().contains("had activity"));
    }

    #[tokio::test]
    async fn test_regression_when_unresolved_with_high_event_count() {
        // Test that an unresolved issue with events above threshold triggers regression
        let body = r#"{
            "id": "791",
            "shortId": "TEST-791",
            "title": "Test Error",
            "count": "10",
            "status": "unresolved",
            "lastSeen": "2024-01-01T00:00:00Z"
        }"#;

        let mock = MockSentryClient::new(200, body);

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let watch = RegressionWatch::new(IssueType::SentryIssue, "791", 1);

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected);
        assert!(result.details.unwrap().contains("10 events"));
    }

    #[tokio::test]
    async fn test_no_regression_for_resolved_issue_with_old_activity() {
        // Resolved issues should not trigger regression based on lastSeen
        // even if lastSeen is after monitoring started (the activity may have occurred
        // before the issue was manually resolved)
        let future_time = Utc::now() + Duration::hours(1);
        let body = format!(
            r#"{{
                "id": "790",
                "shortId": "TEST-790",
                "title": "Test Error",
                "count": "10",
                "status": "resolved",
                "lastSeen": "{}"
            }}"#,
            future_time.to_rfc3339()
        );

        let mock = MockSentryClient::new(200, &body);

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "790", 1);
        watch.monitoring_started_at = Some(Utc::now());

        let result = checker.check_regression(&watch).await.unwrap();
        // Resolved issues should NOT trigger regression based on lastSeen alone
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_issue_not_found() {
        let mock = MockSentryClient::new(404, r#"{"detail": "Not Found"}"#);

        let checker = SentryRegressionChecker::new(create_config(), mock);
        let watch = RegressionWatch::new(IssueType::SentryIssue, "nonexistent", 1);

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
        assert!(result.details.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn test_no_regression_below_threshold() {
        let config = SentryRegressionConfig {
            auth_token: "test-token".to_string(),
            org_slug: "test-org".to_string(),
            event_threshold: 100, // High threshold
        };

        let mock = MockSentryClient::new(
            200,
            r#"{
                "id": "111",
                "shortId": "TEST-111",
                "title": "Test Error",
                "count": "50",
                "status": "unresolved",
                "lastSeen": "2024-01-15T10:00:00Z"
            }"#,
        );

        let checker = SentryRegressionChecker::new(config, mock);
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "111", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::days(2)); // Old monitoring start

        let result = checker.check_regression(&watch).await.unwrap();
        // The last_seen check won't trigger regression since it's before monitoring started
        // and the event count is below threshold
        assert!(!result.regression_detected);
    }
}

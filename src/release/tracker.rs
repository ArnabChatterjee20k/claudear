//! Release tracker for monitoring bug fix propagation.

use crate::error::Result;
use crate::release::ReleaseClient;
use crate::storage::SqliteTracker;
use crate::types::{RegressionWatch, RegressionWatchStatus, ReleaseTracking};
use std::sync::Arc;

/// Configuration for release tracking.
#[derive(Debug, Clone)]
pub struct ReleaseTrackerConfig {
    /// Target repositories to watch for releases (e.g., "appwrite-labs/cloud").
    pub target_repos: Vec<String>,
    /// How often to poll for new releases (in milliseconds).
    pub poll_interval_ms: u64,
}

impl Default for ReleaseTrackerConfig {
    fn default() -> Self {
        Self {
            target_repos: vec![
                "appwrite-labs/cloud".to_string(),
                "appwrite-labs/edge".to_string(),
            ],
            poll_interval_ms: 300_000, // 5 minutes
        }
    }
}

/// Tracks releases to detect when bug fixes are included in production.
pub struct ReleaseTracker<C: crate::github::HttpClient = crate::github::ReqwestHttpClient> {
    client: ReleaseClient<C>,
    tracker: Arc<SqliteTracker>,
    config: ReleaseTrackerConfig,
}

impl ReleaseTracker<crate::github::ReqwestHttpClient> {
    /// Create a new release tracker with the default HTTP client.
    pub fn new(token: impl Into<String>, tracker: Arc<SqliteTracker>) -> Self {
        Self {
            client: ReleaseClient::new(token),
            tracker,
            config: ReleaseTrackerConfig::default(),
        }
    }

    /// Create a new release tracker with custom configuration.
    pub fn with_config(
        token: impl Into<String>,
        tracker: Arc<SqliteTracker>,
        config: ReleaseTrackerConfig,
    ) -> Self {
        Self {
            client: ReleaseClient::new(token),
            tracker,
            config,
        }
    }
}

impl<C: crate::github::HttpClient> ReleaseTracker<C> {
    /// Create a new release tracker with a custom HTTP client.
    pub fn with_http_client(
        client: ReleaseClient<C>,
        tracker: Arc<SqliteTracker>,
        config: ReleaseTrackerConfig,
    ) -> Self {
        Self {
            client,
            tracker,
            config,
        }
    }

    /// Check all watches that are awaiting release and see if they're now included.
    pub async fn check_pending_watches(&self) -> Result<Vec<i64>> {
        let watches = self
            .tracker
            .get_regression_watches_by_status(RegressionWatchStatus::AwaitingRelease)?;

        let mut transitioned = Vec::new();

        for watch in watches {
            if self.check_watch_release(&watch).await? {
                transitioned.push(watch.id);
            }
        }

        Ok(transitioned)
    }

    /// Check if a specific watch's fix is now included in a release.
    async fn check_watch_release(&self, watch: &RegressionWatch) -> Result<bool> {
        // Get the fix attempt to find the PR details
        let attempt = self.tracker.get_attempt_by_id(watch.fix_attempt_id)?;
        let attempt = match attempt {
            Some(a) => a,
            None => return Ok(false),
        };

        // Get the merge commit SHA
        let (repo, pr_number) = match (attempt.github_repo.as_ref(), attempt.github_pr_number) {
            (Some(r), Some(n)) => (r, n),
            _ => return Ok(false),
        };

        let merge_commit = match self.client.get_pr_merge_commit(repo, pr_number).await? {
            Some(sha) => sha,
            None => return Ok(false),
        };

        // Check each target repository for a release containing this commit
        for target_repo in &self.config.target_repos {
            if let Some(release) = self.client.get_latest_release(target_repo).await? {
                // Check if this release contains the fix commit
                if self
                    .client
                    .is_commit_in_release(target_repo, &merge_commit, &release.tag_name)
                    .await?
                {
                    // Record the release tracking
                    let tracking = ReleaseTracking::new(
                        watch.id,
                        &release.tag_name,
                        &release.target_commitish,
                    );
                    self.tracker.record_release_tracking(&tracking)?;

                    // Transition the watch to monitoring status
                    self.tracker
                        .update_regression_watch_status(watch.id, RegressionWatchStatus::Monitoring)?;

                    tracing::info!(
                        watch_id = watch.id,
                        issue_id = %watch.issue_id,
                        release = %release.tag_name,
                        repo = %target_repo,
                        "Fix included in release, starting regression monitoring"
                    );

                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Get the configuration.
    pub fn config(&self) -> &ReleaseTrackerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{HttpClient, HttpResponse};
    use crate::storage::FixAttemptTracker;
    use crate::types::IssueType;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockHttpClient {
        call_count: AtomicUsize,
        responses: Vec<HttpResponse>,
    }

    impl MockHttpClient {
        fn new(responses: Vec<(u16, &str)>) -> Self {
            Self {
                call_count: AtomicUsize::new(0),
                responses: responses
                    .into_iter()
                    .map(|(status, body)| HttpResponse {
                        status,
                        body: body.to_string(),
                    })
                    .collect(),
            }
        }
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn get(&self, _url: &str, _headers: Vec<(&str, String)>) -> Result<HttpResponse> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            if idx < self.responses.len() {
                Ok(HttpResponse {
                    status: self.responses[idx].status,
                    body: self.responses[idx].body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: r#"{"message": "Not Found"}"#.to_string(),
                })
            }
        }
    }

    #[test]
    fn test_release_tracker_config_default() {
        let config = ReleaseTrackerConfig::default();
        assert_eq!(config.target_repos.len(), 2);
        assert!(config.target_repos.contains(&"appwrite-labs/cloud".to_string()));
        assert!(config.target_repos.contains(&"appwrite-labs/edge".to_string()));
        assert_eq!(config.poll_interval_ms, 300_000);
    }

    #[tokio::test]
    async fn test_check_pending_watches_no_watches() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let mock = MockHttpClient::new(vec![]);
        let client = ReleaseClient::with_http_client("test-token", mock);
        let release_tracker = ReleaseTracker::with_http_client(
            client,
            tracker,
            ReleaseTrackerConfig::default(),
        );

        let result = release_tracker.check_pending_watches().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_check_pending_watches_with_watch() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create a fix attempt first
        tracker.record_attempt("sentry", "issue-1", "SENTRY-1").unwrap();
        tracker
            .mark_success("sentry", "issue-1", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_merged("sentry", "issue-1").unwrap();

        // Get the attempt to find its ID
        let attempt = tracker.get_attempt("sentry", "issue-1").unwrap().unwrap();

        // Create a regression watch
        let watch = RegressionWatch::new(IssueType::SentryIssue, "issue-1", attempt.id);
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Mock: PR merge commit, latest release, commit comparison
        let mock = MockHttpClient::new(vec![
            // get_pr_merge_commit
            (200, r#"{"merged": true, "merge_commit_sha": "abc123"}"#),
            // get_latest_release for cloud
            (
                200,
                r#"{
                    "id": 1,
                    "tag_name": "v1.0.0",
                    "name": "Version 1.0.0",
                    "draft": false,
                    "prerelease": false,
                    "created_at": "2024-01-15T10:00:00Z",
                    "published_at": "2024-01-15T10:30:00Z",
                    "target_commitish": "main",
                    "body": "Release notes",
                    "html_url": "https://github.com/appwrite-labs/cloud/releases/tag/v1.0.0"
                }"#,
            ),
            // is_commit_in_release
            (200, r#"{"status": "behind", "ahead_by": 0, "behind_by": 5}"#),
        ]);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release_tracker = ReleaseTracker::with_http_client(
            client,
            tracker.clone(),
            ReleaseTrackerConfig::default(),
        );

        let result = release_tracker.check_pending_watches().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], watch_id);

        // Verify the watch was transitioned to Monitoring
        let updated_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(updated_watch.status, RegressionWatchStatus::Monitoring);
    }
}

//! Release tracker for monitoring bug fix propagation.
//!
//! Supports transitive release tracking through the dependency graph.
//! For example, a fix in `utopia-php/database` flows through:
//! `utopia-php/database` → `appwrite/appwrite` → `appwrite-labs/cloud`
//!
//! Verification method is inferred from dependency type:
//! - Composer → check composer.lock
//! - Npm → check package-lock.json
//! - GitSubmodule → check commit ancestry
//! - Manual → check release_after

use crate::error::Result;
use crate::release::ReleaseClient;
use crate::repo::{DependencyType, RepoRelationships};
use crate::storage::SqliteTracker;
use crate::types::{RegressionWatch, RegressionWatchStatus, ReleaseTracking};
use std::collections::HashMap;
use std::sync::Arc;

/// Configuration for release tracking.
#[derive(Debug, Clone, Default)]
pub struct ReleaseTrackerConfig {
    /// Target repositories to watch for releases.
    pub target_repos: Vec<String>,
    /// How often to poll for new releases (in milliseconds).
    pub poll_interval_ms: u64,
    /// Package name overrides (repo name -> package name).
    pub package_names: HashMap<String, String>,
}

/// Tracks releases to detect when bug fixes are included in production.
pub struct ReleaseTracker<C: crate::github::HttpClient = crate::github::ReqwestHttpClient> {
    client: ReleaseClient<C>,
    tracker: Arc<SqliteTracker>,
    config: ReleaseTrackerConfig,
    /// Dependency graph for tracing fix propagation.
    relationships: RepoRelationships,
}

impl ReleaseTracker<crate::github::ReqwestHttpClient> {
    /// Create a new release tracker with the default HTTP client.
    pub fn new(token: impl Into<String>, tracker: Arc<SqliteTracker>) -> Self {
        Self {
            client: ReleaseClient::new(token),
            tracker,
            config: ReleaseTrackerConfig::default(),
            relationships: RepoRelationships::with_defaults(),
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
            relationships: RepoRelationships::with_defaults(),
        }
    }

    /// Create a new release tracker with custom configuration and relationships.
    pub fn with_relationships(
        token: impl Into<String>,
        tracker: Arc<SqliteTracker>,
        config: ReleaseTrackerConfig,
        relationships: RepoRelationships,
    ) -> Self {
        Self {
            client: ReleaseClient::new(token),
            tracker,
            config,
            relationships,
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
            relationships: RepoRelationships::with_defaults(),
        }
    }

    /// Create a new release tracker with a custom HTTP client and relationships.
    pub fn with_http_client_and_relationships(
        client: ReleaseClient<C>,
        tracker: Arc<SqliteTracker>,
        config: ReleaseTrackerConfig,
        relationships: RepoRelationships,
    ) -> Self {
        Self {
            client,
            tracker,
            config,
            relationships,
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
    ///
    /// Uses the dependency graph to trace fix propagation. For example,
    /// a fix in `utopia-php/database` flows through the graph to `appwrite-labs/cloud`.
    ///
    /// Verification method is inferred from dependency type:
    /// - Composer → check composer.lock
    /// - Npm → check package-lock.json
    /// - GitSubmodule → check commit ancestry
    /// - Manual → check release_after
    async fn check_watch_release(&self, watch: &RegressionWatch) -> Result<bool> {
        // Get the fix attempt to find the PR details
        let attempt = self.tracker.get_attempt_by_id(watch.fix_attempt_id)?;
        let attempt = match attempt {
            Some(a) => a,
            None => return Ok(false),
        };

        // Get the repo and PR number
        let (fix_repo, pr_number) = match (attempt.github_repo.as_ref(), attempt.github_pr_number) {
            (Some(r), Some(n)) => (r.clone(), n),
            _ => return Ok(false),
        };

        // Get PR details including merge time
        let pr_details = match self.client.get_pr_details(&fix_repo, pr_number).await? {
            Some(pr) if pr.merged => pr,
            _ => return Ok(false),
        };

        let merged_at = match &pr_details.merged_at {
            Some(t) => t.clone(),
            None => return Ok(false),
        };

        // Check if this repo is directly a target repo (simple case)
        if self.config.target_repos.contains(&fix_repo) {
            return self
                .check_direct_release(watch, &fix_repo, &pr_details)
                .await;
        }

        // Use the dependency graph to find path to target repos
        // Normalize repo name for graph lookup (strip github.com prefix if present)
        let fix_repo_name = fix_repo
            .trim_start_matches("https://github.com/")
            .to_string();

        // Check each target repo to see if the fix flows to it
        for target_repo in &self.config.target_repos {
            let target_name = target_repo
                .trim_start_matches("https://github.com/")
                .to_string();

            // Check if target depends on fix repo (directly or transitively)
            if self
                .relationships
                .get_graph()
                .depends_on(&target_name, &fix_repo_name)
            {
                // Find the dependency type for the direct dependency edge
                let dep_type = self.find_dependency_type(&fix_repo_name, &target_name);

                if self
                    .check_graph_release(
                        watch,
                        &fix_repo,
                        &merged_at,
                        &pr_details,
                        target_repo,
                        dep_type,
                    )
                    .await?
                {
                    return Ok(true);
                }
            }
        }

        // No path found, fall back to direct check against target repos
        self.check_direct_release_any_target(watch, &fix_repo, &pr_details)
            .await
    }

    /// Find the dependency type from fix repo to target repo.
    /// Returns the type of the first hop in the dependency chain.
    fn find_dependency_type(&self, fix_repo: &str, _target_repo: &str) -> DependencyType {
        // Get dependency type from the first hop
        self.relationships
            .get_graph()
            .get_first_hop_dependency_type(fix_repo)
            .unwrap_or(DependencyType::Manual)
    }

    /// Check if a fix is directly released in the same repo.
    async fn check_direct_release(
        &self,
        watch: &RegressionWatch,
        repo: &str,
        pr_details: &crate::release::PrDetails,
    ) -> Result<bool> {
        let merge_commit = match &pr_details.merge_commit_sha {
            Some(sha) => sha,
            None => return Ok(false),
        };

        if let Some(release) = self.client.get_latest_release(repo).await? {
            if self
                .client
                .is_commit_in_release(repo, merge_commit, &release.tag_name)
                .await?
            {
                return self
                    .transition_to_monitoring(watch, &release.tag_name, repo)
                    .await;
            }
        }

        Ok(false)
    }

    /// Check if a fix is released in any target repo (fallback for unconfigured repos).
    async fn check_direct_release_any_target(
        &self,
        watch: &RegressionWatch,
        fix_repo: &str,
        pr_details: &crate::release::PrDetails,
    ) -> Result<bool> {
        let merge_commit = match &pr_details.merge_commit_sha {
            Some(sha) => sha,
            None => return Ok(false),
        };

        for target_repo in &self.config.target_repos {
            if let Some(release) = self.client.get_latest_release(target_repo).await? {
                // Try direct commit check (works if fix_repo == target_repo or is included)
                if self
                    .client
                    .is_commit_in_release(target_repo, merge_commit, &release.tag_name)
                    .await?
                {
                    tracing::info!(
                        watch_id = watch.id,
                        fix_repo = %fix_repo,
                        target_repo = %target_repo,
                        "Fix commit found directly in target release"
                    );
                    return self
                        .transition_to_monitoring(watch, &release.tag_name, target_repo)
                        .await;
                }
            }
        }

        Ok(false)
    }

    /// Check release through the dependency graph.
    ///
    /// Verification method is inferred from dependency type:
    /// - Composer → check composer.lock
    /// - Npm → check package-lock.json
    /// - GitSubmodule → check commit ancestry
    /// - Manual → check release_after
    async fn check_graph_release(
        &self,
        watch: &RegressionWatch,
        fix_repo: &str,
        merged_at: &str,
        pr_details: &crate::release::PrDetails,
        target_repo: &str,
        dep_type: DependencyType,
    ) -> Result<bool> {
        // Get the latest release in the target repo
        let target_release = match self.client.get_latest_release(target_repo).await? {
            Some(r) => r,
            None => {
                tracing::debug!(
                    watch_id = watch.id,
                    target_repo = %target_repo,
                    "No release found in target repo"
                );
                return Ok(false);
            }
        };

        // Infer verification method from dependency type
        let verified = match dep_type {
            DependencyType::Composer => {
                // Get package name (use override or default to repo name)
                let package_name = self
                    .config
                    .package_names
                    .get(fix_repo)
                    .cloned()
                    .unwrap_or_else(|| fix_repo.to_string());

                self.verify_lock_file(
                    watch,
                    fix_repo,
                    merged_at,
                    target_repo,
                    &target_release.tag_name,
                    &package_name,
                    "composer.lock",
                )
                .await?
            }
            DependencyType::Npm => {
                // Get package name (use override or extract from repo name)
                let package_name = self
                    .config
                    .package_names
                    .get(fix_repo)
                    .cloned()
                    .unwrap_or_else(|| {
                        // npm packages typically use just the repo name part
                        fix_repo
                            .split('/')
                            .next_back()
                            .unwrap_or(fix_repo)
                            .to_string()
                    });

                self.verify_lock_file(
                    watch,
                    fix_repo,
                    merged_at,
                    target_repo,
                    &target_release.tag_name,
                    &package_name,
                    "package-lock.json",
                )
                .await?
            }
            DependencyType::GitSubmodule => {
                self.verify_commit_ancestry(
                    watch,
                    fix_repo,
                    pr_details,
                    target_repo,
                    &target_release.tag_name,
                )
                .await?
            }
            DependencyType::Manual => {
                self.verify_release_after(watch, fix_repo, merged_at, target_repo, &target_release)
                    .await?
            }
        };

        if verified {
            tracing::info!(
                watch_id = watch.id,
                issue_id = %watch.issue_id,
                fix_repo = %fix_repo,
                target_repo = %target_repo,
                release = %target_release.tag_name,
                dep_type = ?dep_type,
                "Fix verified in target release via dependency graph"
            );
            return self
                .transition_to_monitoring(watch, &target_release.tag_name, target_repo)
                .await;
        }

        Ok(false)
    }

    /// Verify fix inclusion using lock file check.
    ///
    /// 1. Find the first release in source repo after the fix was merged
    /// 2. Fetch the lock file from target release
    /// 3. Check if the package version includes the fix version
    #[allow(clippy::too_many_arguments)]
    async fn verify_lock_file(
        &self,
        watch: &RegressionWatch,
        fix_repo: &str,
        merged_at: &str,
        target_repo: &str,
        target_tag: &str,
        package_name: &str,
        lock_file_path: &str,
    ) -> Result<bool> {
        // First, find what version of the source repo includes the fix
        let source_release = match self
            .client
            .get_first_release_after(fix_repo, merged_at)
            .await?
        {
            Some(r) => r,
            None => {
                tracing::debug!(
                    watch_id = watch.id,
                    fix_repo = %fix_repo,
                    "No release found in source repo after fix merge"
                );
                return Ok(false);
            }
        };

        let min_version = source_release.tag_name.clone();

        tracing::debug!(
            watch_id = watch.id,
            fix_repo = %fix_repo,
            min_version = %min_version,
            "Found source release containing fix"
        );

        // Fetch the lock file from the target release
        let lock_content = match self
            .client
            .get_file_at_ref(target_repo, lock_file_path, target_tag)
            .await?
        {
            Some(content) => content,
            None => {
                tracing::warn!(
                    watch_id = watch.id,
                    target_repo = %target_repo,
                    target_tag = %target_tag,
                    lock_file = %lock_file_path,
                    "Lock file not found in target release"
                );
                return Ok(false);
            }
        };

        // Check if the package version in lock file includes the fix
        let version_ok =
            ReleaseClient::<crate::github::ReqwestHttpClient>::check_lock_file_version(
                &lock_content,
                lock_file_path,
                package_name,
                &min_version,
            )?;

        if version_ok {
            tracing::debug!(
                watch_id = watch.id,
                package = %package_name,
                min_version = %min_version,
                target_tag = %target_tag,
                "Package version in lock file includes fix"
            );
        } else {
            tracing::debug!(
                watch_id = watch.id,
                package = %package_name,
                min_version = %min_version,
                target_tag = %target_tag,
                "Package version in lock file does not include fix"
            );
        }

        Ok(version_ok)
    }

    /// Verify fix inclusion by checking if the fix commit is an ancestor of target release.
    ///
    /// This works for repos that are included as git submodules or direct dependencies.
    async fn verify_commit_ancestry(
        &self,
        watch: &RegressionWatch,
        _fix_repo: &str,
        pr_details: &crate::release::PrDetails,
        target_repo: &str,
        target_tag: &str,
    ) -> Result<bool> {
        let merge_commit = match &pr_details.merge_commit_sha {
            Some(sha) => sha,
            None => {
                tracing::debug!(watch_id = watch.id, "PR has no merge commit SHA");
                return Ok(false);
            }
        };

        let is_ancestor = self
            .client
            .is_commit_in_release(target_repo, merge_commit, target_tag)
            .await?;

        if is_ancestor {
            tracing::debug!(
                watch_id = watch.id,
                merge_commit = %merge_commit,
                target_tag = %target_tag,
                "Fix commit is ancestor of target release"
            );
        }

        Ok(is_ancestor)
    }

    /// Verify fix inclusion by checking if target has a release after fix was merged.
    ///
    /// This is the simplest check - assumes any target release after the fix merge
    /// will include the fix. Best used when you know the release process guarantees
    /// dependencies are updated.
    async fn verify_release_after(
        &self,
        watch: &RegressionWatch,
        fix_repo: &str,
        merged_at: &str,
        target_repo: &str,
        target_release: &crate::release::github::GitHubRelease,
    ) -> Result<bool> {
        // First check if source repo has a release after the fix
        let after_time = match self
            .client
            .get_first_release_after(fix_repo, merged_at)
            .await?
        {
            Some(r) => {
                tracing::debug!(
                    watch_id = watch.id,
                    fix_repo = %fix_repo,
                    release = %r.tag_name,
                    "Found release in source repo"
                );
                r.published_at.ok_or_else(|| {
                    crate::error::Error::Other("Release has no published_at".to_string())
                })?
            }
            None => {
                // No release in source repo - use merge time directly
                // This handles repos that don't cut releases
                tracing::debug!(
                    watch_id = watch.id,
                    fix_repo = %fix_repo,
                    "No release in source repo, using merge time"
                );
                merged_at.to_string()
            }
        };

        // Check if target release is after the source release/merge
        let target_published = target_release.published_at.as_ref().ok_or_else(|| {
            crate::error::Error::Other("Target release has no published_at".to_string())
        })?;

        let after_dt = chrono::DateTime::parse_from_rfc3339(&after_time)
            .map_err(|e| crate::error::Error::Other(format!("Invalid timestamp: {}", e)))?;
        let target_dt = chrono::DateTime::parse_from_rfc3339(target_published)
            .map_err(|e| crate::error::Error::Other(format!("Invalid timestamp: {}", e)))?;

        let is_after = target_dt > after_dt;

        if is_after {
            tracing::debug!(
                watch_id = watch.id,
                target_repo = %target_repo,
                target_release = %target_release.tag_name,
                target_published = %target_published,
                after_time = %after_time,
                "Target release is after source fix"
            );
        }

        Ok(is_after)
    }

    /// Transition a watch to monitoring status and record the release.
    async fn transition_to_monitoring(
        &self,
        watch: &RegressionWatch,
        release_tag: &str,
        repo: &str,
    ) -> Result<bool> {
        // Record the release tracking
        let tracking = ReleaseTracking::new(watch.id, release_tag, repo);
        self.tracker.record_release_tracking(&tracking)?;

        // Transition the watch to monitoring status
        self.tracker
            .update_regression_watch_status(watch.id, RegressionWatchStatus::Monitoring)?;

        tracing::info!(
            watch_id = watch.id,
            issue_id = %watch.issue_id,
            release = %release_tag,
            repo = %repo,
            "Fix included in release, starting regression monitoring"
        );

        Ok(true)
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
        // target_repos should be empty by default (configured in YAML)
        assert!(config.target_repos.is_empty());
        assert_eq!(config.poll_interval_ms, 0);
        assert!(config.package_names.is_empty());
    }

    #[tokio::test]
    async fn test_check_pending_watches_no_watches() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let mock = MockHttpClient::new(vec![]);
        let client = ReleaseClient::with_http_client("test-token", mock);
        let release_tracker =
            ReleaseTracker::with_http_client(client, tracker, ReleaseTrackerConfig::default());

        let result = release_tracker.check_pending_watches().await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_check_pending_watches_with_watch() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Create a fix attempt first
        tracker
            .record_attempt("sentry", "issue-1", "SENTRY-1")
            .unwrap();
        tracker
            .mark_success("sentry", "issue-1", "https://github.com/org/repo/pull/42")
            .unwrap();
        tracker.mark_merged("sentry", "issue-1").unwrap();

        // Get the attempt to find its ID
        let attempt = tracker.get_attempt("sentry", "issue-1").unwrap().unwrap();

        // Create a regression watch
        let watch = RegressionWatch::new(IssueType::SentryIssue, "issue-1", attempt.id);
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Mock: PR details, latest release, commit comparison
        let mock = MockHttpClient::new(vec![
            // get_pr_details
            (
                200,
                r#"{"number": 123, "merged": true, "merge_commit_sha": "abc123", "merged_at": "2024-01-10T10:00:00Z"}"#,
            ),
            // get_latest_release for org/repo (target repo)
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
                    "html_url": "https://github.com/org/repo/releases/tag/v1.0.0"
                }"#,
            ),
            // is_commit_in_release
            (
                200,
                r#"{"status": "behind", "ahead_by": 0, "behind_by": 5}"#,
            ),
        ]);

        let client = ReleaseClient::with_http_client("test-token", mock);

        // Configure the tracker with the fix repo as a target repo (direct release case)
        let config = ReleaseTrackerConfig {
            target_repos: vec!["org/repo".to_string()],
            ..Default::default()
        };

        let release_tracker = ReleaseTracker::with_http_client(client, tracker.clone(), config);

        let result = release_tracker.check_pending_watches().await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], watch_id);

        // Verify the watch was transitioned to Monitoring
        let updated_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(updated_watch.status, RegressionWatchStatus::Monitoring);
    }
}

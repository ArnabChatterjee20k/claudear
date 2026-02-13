//! GitHub PR monitoring and issue resolution.

use crate::config::GitHubConfig;
use crate::error::{Error, Result};
use crate::storage::{FixAttemptTracker, SqliteTracker};
use crate::types::{FixAttempt, IssueType, PrReviewRecord, RegressionWatch};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// HTTP response abstraction for testability.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    /// Check if the status is successful (2xx).
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Check if the status is 404 Not Found.
    pub fn is_not_found(&self) -> bool {
        self.status == 404
    }

    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body)
            .map_err(|e| Error::Other(format!("JSON parse error: {}", e)))
    }
}

/// Trait for HTTP client operations to enable testing.
#[async_trait]
pub trait HttpClient: Send + Sync {
    /// Perform a GET request with headers.
    async fn get(&self, url: &str, headers: Vec<(&str, String)>) -> Result<HttpResponse>;
}

/// Default HTTP client using reqwest.
pub struct ReqwestHttpClient {
    client: reqwest::Client,
}

impl ReqwestHttpClient {
    /// Create a new reqwest-based HTTP client.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn get(&self, url: &str, headers: Vec<(&str, String)>) -> Result<HttpResponse> {
        let mut request = self.client.get(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.send().await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }
}

/// GitHub API client for PR monitoring.
pub struct GitHubClient<H: HttpClient = ReqwestHttpClient> {
    config: GitHubConfig,
    http: H,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    state: String,
    merged: bool,
}

/// A GitHub PR review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReview {
    /// Review ID.
    pub id: i64,
    /// Review state (APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED, PENDING).
    pub state: String,
    /// Review body/comment.
    pub body: Option<String>,
    /// Reviewer user.
    pub user: GitHubUser,
    /// When the review was submitted.
    pub submitted_at: Option<String>,
    /// HTML URL to the review.
    pub html_url: Option<String>,
}

/// A GitHub user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    /// User ID.
    pub id: i64,
    /// Username/login.
    pub login: String,
    /// User type (User, Bot, etc.).
    #[serde(rename = "type")]
    pub user_type: Option<String>,
}

/// A repository from the GitHub API (organization repos endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgRepo {
    /// Repository ID.
    pub id: i64,
    /// Full name (org/repo).
    pub full_name: String,
    /// Repository name (without org).
    pub name: String,
    /// Default branch name.
    pub default_branch: String,
    /// Clone URL (HTTPS).
    pub clone_url: String,
    /// SSH URL for cloning.
    #[serde(default)]
    pub ssh_url: String,
    /// HTML URL.
    pub html_url: String,
    /// Whether the repo is private.
    pub private: bool,
    /// Whether the repo is archived.
    pub archived: bool,
}

/// A GitHub PR review comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewComment {
    /// Comment ID.
    pub id: i64,
    /// File path the comment is on.
    pub path: String,
    /// Line position in the diff.
    pub position: Option<i64>,
    /// Original line position.
    pub original_position: Option<i64>,
    /// Comment body.
    pub body: String,
    /// User who wrote the comment.
    pub user: GitHubUser,
    /// When the comment was created.
    pub created_at: String,
    /// When the comment was last updated.
    pub updated_at: String,
    /// HTML URL to the comment.
    pub html_url: String,
    /// Associated review ID if part of a review.
    pub pull_request_review_id: Option<i64>,
    /// Start line (for multi-line comments).
    pub start_line: Option<i64>,
    /// Line number.
    pub line: Option<i64>,
    /// Side of the diff (LEFT or RIGHT).
    pub side: Option<String>,
}

impl GitHubClient<ReqwestHttpClient> {
    /// Create a new GitHub client with the default HTTP client.
    pub fn new(config: GitHubConfig) -> Self {
        Self {
            config,
            http: ReqwestHttpClient::new(),
        }
    }
}

impl<H: HttpClient> GitHubClient<H> {
    /// Create a new GitHub client with a custom HTTP client.
    pub fn with_http_client(config: GitHubConfig, http: H) -> Self {
        Self { config, http }
    }

    /// Check if configured (has token).
    pub fn is_enabled(&self) -> bool {
        self.config.token.is_some()
    }

    /// Get the review trigger tag (e.g., "/claudear").
    pub fn review_trigger(&self) -> &str {
        &self.config.review_trigger
    }

    /// Build standard GitHub API headers.
    fn build_headers(&self, token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("Bearer {}", token)),
            ("Accept", "application/vnd.github+json".to_string()),
            ("User-Agent", "claudear".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ]
    }

    /// Get the status of a PR.
    pub async fn get_pr_status(&self, repo: &str, pr_number: i64) -> Result<PrStatus> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitHub token not configured"))?;

        let url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
        let headers = self.build_headers(token);

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Err(Error::Other(format!(
                "PR not found: {}/pull/{}",
                repo, pr_number
            )));
        }

        if !response.is_success() {
            return Err(Error::Other(format!("GitHub API error: {}", response.body)));
        }

        let pr: PullRequest = response.json()?;

        Ok(if pr.merged {
            PrStatus::Merged
        } else if pr.state == "closed" {
            PrStatus::Closed
        } else {
            PrStatus::Open
        })
    }

    /// Get reviews for a PR.
    pub async fn get_pr_reviews(&self, repo: &str, pr_number: i64) -> Result<Vec<PrReview>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitHub token not configured"))?;

        let url = format!(
            "https://api.github.com/repos/{}/pulls/{}/reviews",
            repo, pr_number
        );
        let headers = self.build_headers(token);

        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Get review comments for a PR.
    pub async fn get_pr_review_comments(
        &self,
        repo: &str,
        pr_number: i64,
    ) -> Result<Vec<PrReviewComment>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitHub token not configured"))?;

        let url = format!(
            "https://api.github.com/repos/{}/pulls/{}/comments",
            repo, pr_number
        );
        let headers = self.build_headers(token);

        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Get reviews for a PR that haven't been processed yet.
    pub async fn get_new_reviews(
        &self,
        repo: &str,
        pr_number: i64,
        since: Option<&str>,
    ) -> Result<Vec<PrReview>> {
        let reviews = self.get_pr_reviews(repo, pr_number).await?;

        if let Some(since_time) = since {
            Ok(reviews
                .into_iter()
                .filter(|r| {
                    r.submitted_at
                        .as_ref()
                        .map(|t| t.as_str() > since_time)
                        .unwrap_or(false)
                })
                .collect())
        } else {
            Ok(reviews)
        }
    }

    /// Get review comments since a given time.
    pub async fn get_new_review_comments(
        &self,
        repo: &str,
        pr_number: i64,
        since: Option<&str>,
    ) -> Result<Vec<PrReviewComment>> {
        let comments = self.get_pr_review_comments(repo, pr_number).await?;

        if let Some(since_time) = since {
            Ok(comments
                .into_iter()
                .filter(|c| c.updated_at.as_str() > since_time)
                .collect())
        } else {
            Ok(comments)
        }
    }

    /// Get the GitHub token (if configured).
    pub fn token(&self) -> Option<&str> {
        self.config.token.as_deref()
    }

    /// List all repositories for an organization.
    ///
    /// Paginates through all results (100 per page) and returns all repos.
    /// Excludes archived repositories.
    pub async fn list_org_repos(&self, org: &str) -> Result<Vec<OrgRepo>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitHub token not configured"))?;

        let mut all_repos = Vec::new();
        let mut page = 1;
        const PER_PAGE: usize = 100;

        loop {
            let url = format!(
                "https://api.github.com/orgs/{}/repos?per_page={}&page={}",
                org, PER_PAGE, page
            );
            let headers = self.build_headers(token);

            let response = self.http.get(&url, headers).await?;

            if response.is_not_found() {
                return Err(Error::Other(format!("Organization not found: {}", org)));
            }

            if !response.is_success() {
                return Err(Error::Other(format!(
                    "GitHub API error ({}): {}",
                    response.status, response.body
                )));
            }

            let repos: Vec<OrgRepo> = response.json()?;
            let count = repos.len();

            // Filter out archived repos
            let active_repos: Vec<OrgRepo> = repos.into_iter().filter(|r| !r.archived).collect();
            all_repos.extend(active_repos);

            // If we got fewer than per_page, we've reached the end
            if count < PER_PAGE {
                break;
            }

            page += 1;

            // Safety limit to prevent infinite loops
            if page > 100 {
                tracing::warn!(org = %org, "Hit pagination limit (100 pages) for org repos");
                break;
            }
        }

        tracing::info!(org = %org, count = all_repos.len(), "Fetched organization repositories");
        Ok(all_repos)
    }
}

/// Status of a GitHub PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Merged,
    Closed,
}

/// PR Monitor that watches for merged PRs and updates issue status.
pub struct PrMonitor<H: HttpClient = ReqwestHttpClient> {
    github: GitHubClient<H>,
    tracker: Arc<dyn FixAttemptTracker>,
    auto_resolve: bool,
    /// Optional SQLite tracker for regression watching.
    /// When set, merged PRs for bug issues will create regression watches
    /// instead of auto-resolving.
    regression_tracker: Option<Arc<SqliteTracker>>,
}

impl PrMonitor<ReqwestHttpClient> {
    /// Create a new PR monitor with the default HTTP client.
    pub fn new(
        github: GitHubClient,
        tracker: Arc<dyn FixAttemptTracker>,
        auto_resolve: bool,
    ) -> Self {
        Self {
            github,
            tracker,
            auto_resolve,
            regression_tracker: None,
        }
    }

    /// Create a new PR monitor with regression tracking enabled.
    pub fn with_regression_tracking(
        github: GitHubClient,
        tracker: Arc<dyn FixAttemptTracker>,
        auto_resolve: bool,
        regression_tracker: Arc<SqliteTracker>,
    ) -> Self {
        Self {
            github,
            tracker,
            auto_resolve,
            regression_tracker: Some(regression_tracker),
        }
    }
}

impl<H: HttpClient> PrMonitor<H> {
    /// Create a new PR monitor with a custom HTTP client.
    pub fn with_http_client(
        github: GitHubClient<H>,
        tracker: Arc<dyn FixAttemptTracker>,
        auto_resolve: bool,
    ) -> Self {
        Self {
            github,
            tracker,
            auto_resolve,
            regression_tracker: None,
        }
    }

    /// Determine if a fix attempt is for a bug-type issue.
    ///
    /// Bug-type issues are:
    /// - All Sentry issues (always bugs)
    /// - Linear issues with a "bug" label (check metadata if available)
    fn is_bug_type(&self, attempt: &FixAttempt) -> bool {
        attempt.is_bug()
    }

    /// Get the issue type for a fix attempt.
    fn get_issue_type(&self, attempt: &FixAttempt) -> IssueType {
        match attempt.source.as_str() {
            "sentry" => IssueType::SentryIssue,
            "linear" => IssueType::LinearBug,
            _ => IssueType::SentryIssue, // Default fallback
        }
    }

    /// Check all pending PRs and update their status.
    pub async fn check_pending_prs(&self) -> Result<Vec<PrStatusUpdate>> {
        if !self.github.is_enabled() {
            return Ok(vec![]);
        }

        let pending_prs = self.tracker.get_pending_prs()?;
        let mut updates = Vec::new();

        for attempt in pending_prs {
            if let Some(update) = self.check_pr(&attempt).await? {
                updates.push(update);
            }
        }

        Ok(updates)
    }

    /// Check a single PR and update its status if needed.
    async fn check_pr(&self, attempt: &FixAttempt) -> Result<Option<PrStatusUpdate>> {
        let repo = match &attempt.github_repo {
            Some(r) => r,
            None => return Ok(None),
        };

        let pr_number = match attempt.github_pr_number {
            Some(n) => n,
            None => return Ok(None),
        };

        let status = match self.github.get_pr_status(repo, pr_number).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    source = "github",
                    repo = %repo,
                    pr_number = pr_number,
                    error = %e,
                    "Failed to check PR"
                );
                return Ok(None);
            }
        };

        match status {
            PrStatus::Merged => {
                tracing::info!(source = "github", repo = %repo, pr_number = pr_number, "PR has been merged!");
                self.tracker
                    .mark_merged(&attempt.source, &attempt.issue_id)?;

                // Log activity event
                let pr_url = attempt.pr_url.clone().unwrap_or_default();
                let activity = crate::types::ActivityLogEntry::new(
                    "pr_merged",
                    format!("PR merged: {}", pr_url),
                )
                .with_source(attempt.source.clone())
                .with_issue(attempt.issue_id.clone(), attempt.short_id.clone())
                .with_metadata(serde_json::json!({
                    "pr_url": pr_url,
                    "repo": repo,
                    "pr_number": pr_number
                }));
                let _ = self.tracker.record_activity(&activity);

                // Determine if we should start regression tracking instead of auto-resolving
                let is_bug = self.is_bug_type(attempt);
                let regression_watch_id = if is_bug {
                    if let Some(ref regression_tracker) = self.regression_tracker {
                        // Create a regression watch for bug-type issues
                        let issue_type = self.get_issue_type(attempt);
                        let mut watch =
                            RegressionWatch::new(issue_type, &attempt.issue_id, attempt.id);
                        watch.pr_merged_at = Some(chrono::Utc::now());

                        match regression_tracker.create_regression_watch(&watch) {
                            Ok(watch_id) => {
                                tracing::info!(
                                    source = "github",
                                    issue_id = %attempt.issue_id,
                                    watch_id = watch_id,
                                    "Created regression watch for bug fix"
                                );

                                // Log activity for regression watch creation
                                let watch_activity = crate::types::ActivityLogEntry::new(
                                    "regression_watch_created",
                                    format!(
                                        "Started regression monitoring for {} after PR merge",
                                        attempt.short_id
                                    ),
                                )
                                .with_source(attempt.source.clone())
                                .with_issue(attempt.issue_id.clone(), attempt.short_id.clone())
                                .with_metadata(serde_json::json!({
                                    "watch_id": watch_id,
                                    "issue_type": issue_type.to_string(),
                                    "pr_url": pr_url
                                }));
                                let _ = self.tracker.record_activity(&watch_activity);

                                Some(watch_id)
                            }
                            Err(e) => {
                                tracing::error!(
                                    source = "github",
                                    issue_id = %attempt.issue_id,
                                    error = %e,
                                    "Failed to create regression watch"
                                );
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // For bugs with regression tracking, don't auto-resolve yet
                // The issue will be resolved after 24 hours of no regressions
                let should_resolve = if regression_watch_id.is_some() {
                    false // Don't auto-resolve, regression monitoring will handle it
                } else {
                    self.auto_resolve
                };

                Ok(Some(PrStatusUpdate {
                    source: attempt.source.clone(),
                    issue_id: attempt.issue_id.clone(),
                    short_id: attempt.short_id.clone(),
                    pr_url,
                    new_status: PrStatus::Merged,
                    should_resolve,
                    regression_watch_id,
                }))
            }
            PrStatus::Closed => {
                tracing::info!(
                    source = "github",
                    repo = %repo,
                    pr_number = pr_number,
                    "PR was closed without merging"
                );
                self.tracker
                    .mark_closed(&attempt.source, &attempt.issue_id)?;

                // Log activity event
                let pr_url = attempt.pr_url.clone().unwrap_or_default();
                let activity = crate::types::ActivityLogEntry::new(
                    "pr_closed",
                    format!("PR closed without merge: {}", pr_url),
                )
                .with_source(attempt.source.clone())
                .with_issue(attempt.issue_id.clone(), attempt.short_id.clone())
                .with_metadata(serde_json::json!({
                    "pr_url": pr_url,
                    "repo": repo,
                    "pr_number": pr_number
                }));
                let _ = self.tracker.record_activity(&activity);

                Ok(Some(PrStatusUpdate {
                    source: attempt.source.clone(),
                    issue_id: attempt.issue_id.clone(),
                    short_id: attempt.short_id.clone(),
                    pr_url,
                    new_status: PrStatus::Closed,
                    should_resolve: false,
                    regression_watch_id: None,
                }))
            }
            PrStatus::Open => Ok(None),
        }
    }
}

/// Update information for a PR status change.
#[derive(Debug, Clone)]
pub struct PrStatusUpdate {
    pub source: String,
    pub issue_id: String,
    pub short_id: String,
    pub pr_url: String,
    pub new_status: PrStatus,
    /// Whether the issue should be resolved on the source.
    pub should_resolve: bool,
    /// If set, a regression watch was created for this bug fix.
    /// The issue won't be auto-resolved until regression monitoring completes.
    pub regression_watch_id: Option<i64>,
}

/// State for tracking PR reviews.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewState {
    /// PR URL.
    pub pr_url: String,
    /// Repository (owner/repo).
    pub repo: String,
    /// PR number.
    pub pr_number: i64,
    /// Source issue ID.
    pub issue_id: String,
    /// Source (linear, sentry, etc.).
    pub source: String,
    /// Last review ID processed.
    pub last_review_id: Option<i64>,
    /// Last review timestamp processed.
    pub last_review_time: Option<String>,
    /// Last comment ID processed.
    pub last_comment_id: Option<i64>,
    /// Last comment timestamp processed.
    pub last_comment_time: Option<String>,
    /// Whether this PR is still being watched.
    pub is_active: bool,
}

impl PrReviewState {
    /// Create a new review state.
    pub fn new(
        pr_url: impl Into<String>,
        repo: impl Into<String>,
        pr_number: i64,
        issue_id: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            pr_url: pr_url.into(),
            repo: repo.into(),
            pr_number,
            issue_id: issue_id.into(),
            source: source.into(),
            last_review_id: None,
            last_review_time: None,
            last_comment_id: None,
            last_comment_time: None,
            is_active: true,
        }
    }
}

/// An event from a PR review watcher.
#[derive(Debug, Clone)]
pub enum ReviewEvent {
    /// A new review was submitted.
    ReviewSubmitted {
        pr_url: String,
        repo: String,
        pr_number: i64,
        review: PrReview,
        /// Inline comments submitted as part of this review.
        inline_comments: Vec<PrReviewComment>,
    },
    /// New review comments were added.
    CommentsAdded {
        pr_url: String,
        repo: String,
        pr_number: i64,
        comments: Vec<PrReviewComment>,
    },
}

impl ReviewEvent {
    /// Get the PR URL for this event.
    pub fn pr_url(&self) -> &str {
        match self {
            ReviewEvent::ReviewSubmitted { pr_url, .. } => pr_url,
            ReviewEvent::CommentsAdded { pr_url, .. } => pr_url,
        }
    }

    /// Check if this event requires agent action.
    pub fn requires_action(&self) -> bool {
        match self {
            ReviewEvent::ReviewSubmitted { review, .. } => {
                // Only process reviews that request changes or have comments
                matches!(
                    review.state.to_uppercase().as_str(),
                    "CHANGES_REQUESTED" | "COMMENTED"
                )
            }
            ReviewEvent::CommentsAdded { comments, .. } => !comments.is_empty(),
        }
    }

    /// Get a summary of feedback for the agent.
    pub fn get_feedback_summary(&self) -> String {
        match self {
            ReviewEvent::ReviewSubmitted {
                review,
                inline_comments,
                ..
            } => {
                let mut summary = format!(
                    "Review from @{} ({})\n",
                    review.user.login,
                    review.state.to_uppercase()
                );
                if let Some(body) = &review.body {
                    if !body.is_empty() {
                        summary.push_str(&format!("\nReview comment:\n{}\n", body));
                    }
                }
                if !inline_comments.is_empty() {
                    summary.push_str(&format!("\nInline comments ({}):\n", inline_comments.len()));
                    for comment in inline_comments {
                        summary.push_str(&format!("- `{}`", comment.path));
                        if let Some(line) = comment.line {
                            summary.push_str(&format!(" (line {})", line));
                        }
                        summary.push_str(&format!(": {}\n", comment.body));
                    }
                }
                summary
            }
            ReviewEvent::CommentsAdded { comments, .. } => {
                let mut summary = String::new();
                for comment in comments {
                    summary.push_str(&format!(
                        "Comment from @{} on `{}`",
                        comment.user.login, comment.path
                    ));
                    if let Some(line) = comment.line {
                        summary.push_str(&format!(" (line {})", line));
                    }
                    summary.push_str(&format!(":\n{}\n\n", comment.body));
                }
                summary
            }
        }
    }
}

/// Watches PRs for review activity.
pub struct ReviewWatcher<H: HttpClient = ReqwestHttpClient> {
    github: GitHubClient<H>,
    /// Map of PR URL -> review state
    states: std::sync::RwLock<std::collections::HashMap<String, PrReviewState>>,
    /// Optional tracker for recording reviews to the database
    tracker: Option<Arc<dyn FixAttemptTracker>>,
    /// Optional sqlite tracker for persisting review states
    sqlite_tracker: Option<Arc<SqliteTracker>>,
}

impl ReviewWatcher<ReqwestHttpClient> {
    /// Create a new review watcher with the default HTTP client.
    pub fn new(github: GitHubClient) -> Self {
        Self {
            github,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: None,
            sqlite_tracker: None,
        }
    }

    /// Create a new review watcher with a tracker for analytics.
    pub fn with_tracker(github: GitHubClient, tracker: Arc<dyn FixAttemptTracker>) -> Self {
        Self {
            github,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: Some(tracker),
            sqlite_tracker: None,
        }
    }

    /// Create a new review watcher with a sqlite tracker for state persistence.
    pub fn with_sqlite_tracker(
        github: GitHubClient,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<SqliteTracker>>,
    ) -> Self {
        Self {
            github,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: Some(tracker),
            sqlite_tracker,
        }
    }
}

impl<H: HttpClient> ReviewWatcher<H> {
    /// Create a new review watcher with a custom HTTP client.
    pub fn with_http_client(github: GitHubClient<H>) -> Self {
        Self {
            github,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: None,
            sqlite_tracker: None,
        }
    }

    /// Create a new review watcher with a custom HTTP client and tracker.
    pub fn with_http_client_and_tracker(
        github: GitHubClient<H>,
        tracker: Arc<dyn FixAttemptTracker>,
    ) -> Self {
        Self {
            github,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: Some(tracker),
            sqlite_tracker: None,
        }
    }

    /// Check if the watcher is enabled.
    pub fn is_enabled(&self) -> bool {
        self.github.is_enabled()
    }

    /// Start watching a PR for reviews.
    pub fn watch_pr(&self, state: PrReviewState) {
        // Persist to database first if sqlite_tracker is available
        if let Some(ref sqlite) = self.sqlite_tracker {
            if let Err(e) = sqlite.save_pr_review_state(&state) {
                tracing::warn!(
                    component = "review_watcher",
                    pr_url = %state.pr_url,
                    error = %e,
                    "Failed to persist PR review state to database"
                );
            }
        }

        let mut states = self.states.write().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        states.insert(state.pr_url.clone(), state);
    }

    /// Stop watching a PR.
    pub fn unwatch_pr(&self, pr_url: &str) {
        // Deactivate in database first if sqlite_tracker is available
        if let Some(ref sqlite) = self.sqlite_tracker {
            if let Err(e) = sqlite.deactivate_pr_review_state(pr_url) {
                tracing::warn!(
                    component = "review_watcher",
                    pr_url = %pr_url,
                    error = %e,
                    "Failed to deactivate PR review state in database"
                );
            }
        }

        let mut states = self.states.write().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        if let Some(state) = states.get_mut(pr_url) {
            state.is_active = false;
        }
    }

    /// Get the state for a PR.
    pub fn get_state(&self, pr_url: &str) -> Option<PrReviewState> {
        let states = self.states.read().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        states.get(pr_url).cloned()
    }

    /// Get all active states.
    pub fn get_active_states(&self) -> Vec<PrReviewState> {
        let states = self.states.read().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        states.values().filter(|s| s.is_active).cloned().collect()
    }

    /// Load states from storage.
    pub fn load_states(&self, states_vec: Vec<PrReviewState>) {
        let mut states = self.states.write().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        for state in states_vec {
            if state.is_active {
                states.insert(state.pr_url.clone(), state);
            }
        }
    }

    /// Get all states for persistence.
    pub fn get_all_states(&self) -> Vec<PrReviewState> {
        let states = self.states.read().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });
        states.values().cloned().collect()
    }

    /// Check all watched PRs for new reviews.
    pub async fn check_for_reviews(&self) -> Result<Vec<ReviewEvent>> {
        if !self.github.is_enabled() {
            return Ok(vec![]);
        }

        let states_to_check: Vec<PrReviewState> = {
            let states = self.states.read().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            states.values().filter(|s| s.is_active).cloned().collect()
        };

        let mut events = Vec::new();

        for state in states_to_check {
            match self.check_pr_reviews(&state).await {
                Ok(pr_events) => events.extend(pr_events),
                Err(e) => {
                    tracing::warn!(
                        component = "review_watcher",
                        pr_url = %state.pr_url,
                        error = %e,
                        "Failed to check reviews"
                    );
                }
            }
        }

        Ok(events)
    }

    /// Record a review to the database for analytics.
    fn record_review_to_db(&self, state: &PrReviewState, review: &PrReview) {
        if let Some(ref tracker) = self.tracker {
            let mut record = PrReviewRecord::new(&state.pr_url);
            record.reviewer = Some(review.user.login.clone());
            record.review_state = Some(review.state.clone());
            record.body = review.body.clone();

            // Parse the submitted_at timestamp from GitHub's ISO 8601 format
            if let Some(ref ts) = review.submitted_at {
                if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) {
                    record.submitted_at = Some(parsed.with_timezone(&chrono::Utc));
                }
            }

            if let Err(e) = tracker.record_pr_review(&record) {
                tracing::warn!(
                    component = "review_watcher",
                    pr_url = %state.pr_url,
                    error = %e,
                    "Failed to record review to database"
                );
            }

            // Log activity event for the review
            let message = format!("PR review from {}: {}", review.user.login, review.state);
            let activity = crate::types::ActivityLogEntry::new("pr_review_received", &message)
                .with_source("github".to_string())
                .with_metadata(serde_json::json!({
                    "pr_url": state.pr_url,
                    "reviewer": review.user.login,
                    "review_state": review.state,
                    "repo": state.repo,
                    "pr_number": state.pr_number
                }));

            if let Err(e) = tracker.record_activity(&activity) {
                tracing::warn!(
                    component = "review_watcher",
                    error = %e,
                    "Failed to record review activity"
                );
            }
        }
    }

    /// Check a single PR for new reviews.
    async fn check_pr_reviews(&self, state: &PrReviewState) -> Result<Vec<ReviewEvent>> {
        let mut events = Vec::new();

        // Check for new reviews
        let reviews = self
            .github
            .get_new_reviews(
                &state.repo,
                state.pr_number,
                state.last_review_time.as_deref(),
            )
            .await?;

        // Collect review IDs we're processing this cycle so we can
        // attach their inline comments directly to the review event.
        let mut processed_review_ids: Vec<i64> = Vec::new();

        for review in reviews {
            // Skip reviews we've already processed
            if let Some(last_id) = state.last_review_id {
                if review.id <= last_id {
                    continue;
                }
            }

            // Skip bot reviews
            if review.user.user_type.as_deref() == Some("Bot") {
                continue;
            }

            // Skip pending reviews (not yet submitted)
            if review.state.to_uppercase() == "PENDING" {
                continue;
            }

            // Record the review to the database for analytics
            self.record_review_to_db(state, &review);

            processed_review_ids.push(review.id);

            events.push(ReviewEvent::ReviewSubmitted {
                pr_url: state.pr_url.clone(),
                repo: state.repo.clone(),
                pr_number: state.pr_number,
                review: review.clone(),
                inline_comments: Vec::new(), // populated below
            });

            // Update state
            let mut states = self.states.write().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(s) = states.get_mut(&state.pr_url) {
                s.last_review_id = Some(review.id);
                s.last_review_time = review.submitted_at.clone();

                // Persist state update to database
                if let Some(ref sqlite) = self.sqlite_tracker {
                    if let Err(e) = sqlite.save_pr_review_state(s) {
                        tracing::warn!(
                            component = "review_watcher",
                            pr_url = %s.pr_url,
                            error = %e,
                            "Failed to persist PR review state update"
                        );
                    }
                }
            }
        }

        // Check for new comments
        let comments = self
            .github
            .get_new_review_comments(
                &state.repo,
                state.pr_number,
                state.last_comment_time.as_deref(),
            )
            .await?;

        // Get the review trigger (e.g., "/claudear")
        let trigger = self.github.review_trigger();

        // Filter out comments we've already processed
        let new_comments: Vec<_> = comments
            .into_iter()
            .filter(|c| {
                if let Some(last_id) = state.last_comment_id {
                    c.id > last_id
                } else {
                    true
                }
            })
            .filter(|c| c.user.user_type.as_deref() != Some("Bot"))
            .collect();

        // Attach inline comments to their parent review events (these bypass
        // the trigger filter since they were submitted as part of the review).
        // Standalone comments (not part of a review we just processed) still
        // require the trigger.
        let mut attached_comment_ids: std::collections::HashSet<i64> =
            std::collections::HashSet::new();

        for comment in &new_comments {
            if let Some(review_id) = comment.pull_request_review_id {
                if processed_review_ids.contains(&review_id) {
                    // Find the matching review event and attach this comment
                    for event in &mut events {
                        if let ReviewEvent::ReviewSubmitted {
                            review,
                            inline_comments,
                            ..
                        } = event
                        {
                            if review.id == review_id {
                                inline_comments.push(comment.clone());
                                attached_comment_ids.insert(comment.id);
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Standalone comments: not attached to a review we just processed,
        // must match the trigger filter
        let standalone_comments: Vec<_> = new_comments
            .into_iter()
            .filter(|c| !attached_comment_ids.contains(&c.id))
            .filter(|c| {
                if trigger.is_empty() {
                    true
                } else {
                    c.body.to_lowercase().contains(&trigger.to_lowercase())
                }
            })
            .collect();

        // Combine all new comments (attached + standalone) for state tracking
        let all_new_comment_ids: Vec<i64> = attached_comment_ids
            .iter()
            .copied()
            .chain(standalone_comments.iter().map(|c| c.id))
            .collect();

        if !all_new_comment_ids.is_empty() {
            // Record all new comments to database
            if let Some(ref sqlite) = self.sqlite_tracker {
                // Re-collect for recording: attached comments are already in events,
                // standalone comments are in standalone_comments
                for event in &events {
                    if let ReviewEvent::ReviewSubmitted {
                        inline_comments, ..
                    } = event
                    {
                        for comment in inline_comments {
                            if let Err(e) = sqlite.record_pr_review_comment(&state.pr_url, comment)
                            {
                                tracing::warn!(
                                    component = "review_watcher",
                                    pr_url = %state.pr_url,
                                    comment_id = comment.id,
                                    error = %e,
                                    "Failed to record PR review comment"
                                );
                            }
                        }
                    }
                }
                for comment in &standalone_comments {
                    if let Err(e) = sqlite.record_pr_review_comment(&state.pr_url, comment) {
                        tracing::warn!(
                            component = "review_watcher",
                            pr_url = %state.pr_url,
                            comment_id = comment.id,
                            error = %e,
                            "Failed to record PR review comment"
                        );
                    }
                }
            }

            // Update state with latest comment
            let max_id = all_new_comment_ids.iter().copied().max().unwrap();
            let latest_time = {
                let mut latest: Option<String> = None;
                for event in &events {
                    if let ReviewEvent::ReviewSubmitted {
                        inline_comments, ..
                    } = event
                    {
                        for c in inline_comments {
                            if c.id == max_id {
                                latest = Some(c.updated_at.clone());
                            }
                        }
                    }
                }
                if latest.is_none() {
                    for c in &standalone_comments {
                        if c.id == max_id {
                            latest = Some(c.updated_at.clone());
                        }
                    }
                }
                latest
            };

            let mut states = self.states.write().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(s) = states.get_mut(&state.pr_url) {
                s.last_comment_id = Some(max_id);
                if let Some(t) = latest_time {
                    s.last_comment_time = Some(t);
                }

                // Persist state update to database
                if let Some(ref sqlite) = self.sqlite_tracker {
                    if let Err(e) = sqlite.save_pr_review_state(s) {
                        tracing::warn!(
                            component = "review_watcher",
                            pr_url = %s.pr_url,
                            error = %e,
                            "Failed to persist PR review state update"
                        );
                    }
                }
            }
        }

        if !standalone_comments.is_empty() {
            events.push(ReviewEvent::CommentsAdded {
                pr_url: state.pr_url.clone(),
                repo: state.repo.clone(),
                pr_number: state.pr_number,
                comments: standalone_comments,
            });
        }

        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing.
    #[allow(clippy::type_complexity)]
    pub struct MockHttpClient {
        responses: Mutex<HashMap<String, HttpResponse>>,
        requests: Mutex<Vec<(String, Vec<(String, String)>)>>,
    }

    impl MockHttpClient {
        pub fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
            }
        }

        /// Add a mock response for a URL.
        pub fn mock_response(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            let mut responses = self.responses.lock().unwrap();
            responses.insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        /// Get recorded requests.
        pub fn get_requests(&self) -> Vec<(String, Vec<(String, String)>)> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn get(&self, url: &str, headers: Vec<(&str, String)>) -> Result<HttpResponse> {
            // Record the request
            let owned_headers: Vec<(String, String)> = headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect();
            self.requests
                .lock()
                .unwrap()
                .push((url.to_string(), owned_headers));

            // Return mock response
            let responses = self.responses.lock().unwrap();
            if let Some(response) = responses.get(url) {
                Ok(HttpResponse {
                    status: response.status,
                    body: response.body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not found".to_string(),
                })
            }
        }
    }

    #[test]
    fn test_http_response_is_success() {
        let response = HttpResponse {
            status: 200,
            body: "{}".to_string(),
        };
        assert!(response.is_success());

        let response = HttpResponse {
            status: 201,
            body: "{}".to_string(),
        };
        assert!(response.is_success());

        let response = HttpResponse {
            status: 404,
            body: "{}".to_string(),
        };
        assert!(!response.is_success());

        let response = HttpResponse {
            status: 500,
            body: "{}".to_string(),
        };
        assert!(!response.is_success());
    }

    #[test]
    fn test_http_response_is_not_found() {
        let response = HttpResponse {
            status: 404,
            body: "{}".to_string(),
        };
        assert!(response.is_not_found());

        let response = HttpResponse {
            status: 200,
            body: "{}".to_string(),
        };
        assert!(!response.is_not_found());
    }

    #[test]
    fn test_http_response_json() {
        let response = HttpResponse {
            status: 200,
            body: r#"{"name": "test"}"#.to_string(),
        };
        let parsed: serde_json::Value = response.json().unwrap();
        assert_eq!(parsed["name"], "test");
    }

    #[test]
    fn test_http_response_json_error() {
        let response = HttpResponse {
            status: 200,
            body: "invalid json".to_string(),
        };
        let result: Result<serde_json::Value> = response.json();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_pr_status_open() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1",
            200,
            r#"{"number": 1, "state": "open", "merged": false}"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let status = client.get_pr_status("owner/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Open);
    }

    #[tokio::test]
    async fn test_get_pr_status_merged() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1",
            200,
            r#"{"number": 1, "state": "closed", "merged": true}"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let status = client.get_pr_status("owner/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Merged);
    }

    #[tokio::test]
    async fn test_get_pr_status_closed() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1",
            200,
            r#"{"number": 1, "state": "closed", "merged": false}"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let status = client.get_pr_status("owner/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Closed);
    }

    #[tokio::test]
    async fn test_get_pr_status_not_found() {
        let mock = MockHttpClient::new();
        // No mock response means 404

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.get_pr_status("owner/repo", 1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("PR not found"));
    }

    #[tokio::test]
    async fn test_get_pr_status_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1",
            500,
            "Internal Server Error",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.get_pr_status("owner/repo", 1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("GitHub API error"));
    }

    #[tokio::test]
    async fn test_get_pr_status_no_token() {
        let mock = MockHttpClient::new();
        let config = GitHubConfig::default(); // No token
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.get_pr_status("owner/repo", 1).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("token"));
    }

    #[tokio::test]
    async fn test_get_pr_reviews() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "APPROVED", "body": "LGTM", "user": {"id": 123, "login": "reviewer"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let reviews = client.get_pr_reviews("owner/repo", 1).await.unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].state, "APPROVED");
    }

    #[tokio::test]
    async fn test_get_pr_reviews_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            403,
            "Forbidden",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.get_pr_reviews("owner/repo", 1).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_pr_review_comments() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            r#"[
                {
                    "id": 1,
                    "path": "src/main.rs",
                    "body": "Please fix this",
                    "user": {"id": 123, "login": "reviewer"},
                    "created_at": "2024-01-01T00:00:00Z",
                    "updated_at": "2024-01-01T00:00:00Z",
                    "html_url": "https://github.com/test"
                }
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let comments = client
            .get_pr_review_comments("owner/repo", 1)
            .await
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].path, "src/main.rs");
    }

    #[tokio::test]
    async fn test_get_new_reviews_filters_by_time() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "APPROVED", "body": null, "user": {"id": 123, "login": "old"}, "submitted_at": "2024-01-01T00:00:00Z"},
                {"id": 2, "state": "CHANGES_REQUESTED", "body": null, "user": {"id": 124, "login": "new"}, "submitted_at": "2024-01-02T00:00:00Z"}
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        // Filter to only reviews after 2024-01-01T12:00:00Z
        let reviews = client
            .get_new_reviews("owner/repo", 1, Some("2024-01-01T12:00:00Z"))
            .await
            .unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].user.login, "new");
    }

    #[tokio::test]
    async fn test_get_new_reviews_no_filter() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "APPROVED", "body": null, "user": {"id": 123, "login": "r1"}},
                {"id": 2, "state": "CHANGES_REQUESTED", "body": null, "user": {"id": 124, "login": "r2"}}
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let reviews = client.get_new_reviews("owner/repo", 1, None).await.unwrap();
        assert_eq!(reviews.len(), 2);
    }

    #[tokio::test]
    async fn test_get_new_review_comments_filters_by_time() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            r#"[
                {"id": 1, "path": "a.rs", "body": "old", "user": {"id": 1, "login": "u"}, "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z", "html_url": "url"},
                {"id": 2, "path": "b.rs", "body": "new", "user": {"id": 1, "login": "u"}, "created_at": "2024-01-02T00:00:00Z", "updated_at": "2024-01-02T00:00:00Z", "html_url": "url"}
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let comments = client
            .get_new_review_comments("owner/repo", 1, Some("2024-01-01T12:00:00Z"))
            .await
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].path, "b.rs");
    }

    #[tokio::test]
    async fn test_request_includes_auth_headers() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1",
            200,
            r#"{"number": 1, "state": "open", "merged": false}"#,
        );

        let config = GitHubConfig {
            token: Some("my_secret_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);

        let _ = client.get_pr_status("owner/repo", 1).await;

        let http = &client.http;
        let requests = http.get_requests();
        assert_eq!(requests.len(), 1);

        let (_, headers) = &requests[0];
        let auth_header = headers.iter().find(|(k, _)| k == "Authorization");
        assert!(auth_header.is_some());
        assert!(auth_header.unwrap().1.contains("my_secret_token"));
    }

    #[tokio::test]
    async fn test_review_watcher_check_for_reviews_with_mock() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "CHANGES_REQUESTED", "body": "Fix this", "user": {"id": 123, "login": "reviewer", "type": "User"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            "[]",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        let state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let events = watcher.check_for_reviews().await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ReviewEvent::ReviewSubmitted { review, .. } => {
                assert_eq!(review.state, "CHANGES_REQUESTED");
            }
            _ => panic!("Expected ReviewSubmitted event"),
        }
    }

    #[tokio::test]
    async fn test_review_watcher_skips_bot_reviews() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "APPROVED", "body": null, "user": {"id": 123, "login": "bot", "type": "Bot"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            "[]",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        let state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let events = watcher.check_for_reviews().await.unwrap();
        assert!(events.is_empty()); // Bot reviews are skipped
    }

    #[tokio::test]
    async fn test_review_watcher_skips_pending_reviews() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "PENDING", "body": null, "user": {"id": 123, "login": "user", "type": "User"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            "[]",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        let state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let events = watcher.check_for_reviews().await.unwrap();
        assert!(events.is_empty()); // Pending reviews are skipped
    }

    #[tokio::test]
    async fn test_review_watcher_tracks_review_state() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 5, "state": "APPROVED", "body": null, "user": {"id": 123, "login": "user", "type": "User"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            "[]",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        let state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let _ = watcher.check_for_reviews().await.unwrap();

        // Check that the state was updated with the latest review ID
        let updated_state = watcher.get_state("url").unwrap();
        assert_eq!(updated_state.last_review_id, Some(5));
    }

    #[tokio::test]
    async fn test_review_watcher_comments_added_event() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            "[]",
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            r#"[
                {"id": 1, "path": "file.rs", "body": "/claudear Fix this", "user": {"id": 123, "login": "user", "type": "User"}, "created_at": "2024-01-01T00:00:00Z", "updated_at": "2024-01-01T00:00:00Z", "html_url": "url"}
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        let state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let events = watcher.check_for_reviews().await.unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ReviewEvent::CommentsAdded { comments, .. } => {
                assert_eq!(comments.len(), 1);
                assert_eq!(comments[0].body, "/claudear Fix this");
            }
            _ => panic!("Expected CommentsAdded event"),
        }
    }

    #[tokio::test]
    async fn test_review_watcher_skips_already_processed_reviews() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/reviews",
            200,
            r#"[
                {"id": 1, "state": "APPROVED", "body": null, "user": {"id": 123, "login": "user", "type": "User"}, "submitted_at": "2024-01-01T00:00:00Z"}
            ]"#,
        );
        mock.mock_response(
            "https://api.github.com/repos/owner/repo/pulls/1/comments",
            200,
            "[]",
        );

        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::with_http_client(config, mock);
        let watcher = ReviewWatcher::with_http_client(client);

        // Pre-set the last_review_id to 1, so the review should be skipped
        let mut state = PrReviewState::new("url", "owner/repo", 1, "issue", "linear");
        state.last_review_id = Some(1);
        watcher.watch_pr(state);

        let events = watcher.check_for_reviews().await.unwrap();
        assert!(events.is_empty()); // Review was already processed
    }

    #[test]
    fn test_client_not_enabled_without_token() {
        let config = GitHubConfig::default();
        let client = GitHubClient::new(config);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_client_enabled_with_token() {
        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        assert!(client.is_enabled());
    }

    #[test]
    fn test_pr_review_state_new() {
        let state = PrReviewState::new(
            "https://github.com/owner/repo/pull/1",
            "owner/repo",
            1,
            "issue-123",
            "linear",
        );

        assert_eq!(state.pr_url, "https://github.com/owner/repo/pull/1");
        assert_eq!(state.repo, "owner/repo");
        assert_eq!(state.pr_number, 1);
        assert!(state.is_active);
        assert!(state.last_review_id.is_none());
    }

    #[test]
    fn test_review_event_pr_url() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: Some("LGTM".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: Some("https://github.com/test".to_string()),
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "https://github.com/test/pull/1".to_string(),
            repo: "test/test".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        assert_eq!(event.pr_url(), "https://github.com/test/pull/1");
    }

    #[test]
    fn test_review_event_requires_action() {
        let approved_review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: None,
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let changes_requested = PrReview {
            id: 2,
            state: "CHANGES_REQUESTED".to_string(),
            body: Some("Please fix this".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let approved_event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: approved_review,
            inline_comments: vec![],
        };

        let changes_event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: changes_requested,
            inline_comments: vec![],
        };

        // Approved reviews don't require action
        assert!(!approved_event.requires_action());
        // Changes requested do require action
        assert!(changes_event.requires_action());
    }

    #[test]
    fn test_review_event_feedback_summary() {
        let review = PrReview {
            id: 1,
            state: "CHANGES_REQUESTED".to_string(),
            body: Some("Please add tests".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("CHANGES_REQUESTED"));
        assert!(summary.contains("Please add tests"));
    }

    #[test]
    fn test_review_event_comments_summary() {
        let comments = vec![PrReviewComment {
            id: 1,
            path: "src/main.rs".to_string(),
            position: None,
            original_position: None,
            body: "This should use a match statement".to_string(),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "https://github.com/test".to_string(),
            pull_request_review_id: Some(1),
            start_line: None,
            line: Some(42),
            side: Some("RIGHT".to_string()),
        }];

        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments,
        };

        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("src/main.rs"));
        assert!(summary.contains("line 42"));
        assert!(summary.contains("match statement"));
    }

    #[test]
    fn test_review_watcher_watch_unwatch() {
        let config = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        let state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        watcher.watch_pr(state);

        let retrieved = watcher.get_state("url");
        assert!(retrieved.is_some());
        assert!(retrieved.unwrap().is_active);

        watcher.unwatch_pr("url");
        let after_unwatch = watcher.get_state("url");
        assert!(after_unwatch.is_some());
        assert!(!after_unwatch.unwrap().is_active);
    }

    #[test]
    fn test_review_watcher_active_states() {
        let config = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        let state1 = PrReviewState::new("url1", "repo", 1, "issue1", "linear");
        let state2 = PrReviewState::new("url2", "repo", 2, "issue2", "linear");

        watcher.watch_pr(state1);
        watcher.watch_pr(state2);
        watcher.unwatch_pr("url1");

        let active = watcher.get_active_states();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].pr_url, "url2");
    }

    #[test]
    fn test_review_watcher_load_states() {
        let config = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        let mut inactive_state = PrReviewState::new("url1", "repo", 1, "issue1", "linear");
        inactive_state.is_active = false;

        let active_state = PrReviewState::new("url2", "repo", 2, "issue2", "linear");

        watcher.load_states(vec![inactive_state, active_state]);

        // Only active states should be loaded
        let all = watcher.get_all_states();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].pr_url, "url2");
    }

    #[test]
    fn test_pr_status_equality() {
        assert_eq!(PrStatus::Open, PrStatus::Open);
        assert_eq!(PrStatus::Merged, PrStatus::Merged);
        assert_eq!(PrStatus::Closed, PrStatus::Closed);
        assert_ne!(PrStatus::Open, PrStatus::Merged);
    }

    #[test]
    fn test_client_token_accessor() {
        let config = GitHubConfig {
            token: Some("my_token".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        assert_eq!(client.token(), Some("my_token"));
    }

    #[test]
    fn test_client_token_none() {
        let config = GitHubConfig::default();
        let client = GitHubClient::new(config);
        assert!(client.token().is_none());
    }

    #[test]
    fn test_review_event_empty_comments() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn test_review_event_commented_state() {
        let review = PrReview {
            id: 1,
            state: "COMMENTED".to_string(),
            body: Some("Some feedback".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        assert!(event.requires_action());
    }

    #[test]
    fn test_review_event_dismissed() {
        let review = PrReview {
            id: 1,
            state: "DISMISSED".to_string(),
            body: None,
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        assert!(!event.requires_action());
    }

    #[test]
    fn test_pr_review_state_update_review_tracking() {
        let mut state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        assert!(state.last_review_id.is_none());
        assert!(state.last_review_time.is_none());

        state.last_review_id = Some(123);
        state.last_review_time = Some("2024-01-01T00:00:00Z".to_string());

        assert_eq!(state.last_review_id, Some(123));
        assert_eq!(
            state.last_review_time,
            Some("2024-01-01T00:00:00Z".to_string())
        );
    }

    #[test]
    fn test_pr_review_state_update_comment_tracking() {
        let mut state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        assert!(state.last_comment_id.is_none());

        state.last_comment_id = Some(456);
        state.last_comment_time = Some("2024-01-01T01:00:00Z".to_string());

        assert_eq!(state.last_comment_id, Some(456));
    }

    #[test]
    fn test_pr_status_update_fields() {
        let update = PrStatusUpdate {
            source: "linear".to_string(),
            issue_id: "issue-1".to_string(),
            short_id: "LIN-1".to_string(),
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            new_status: PrStatus::Merged,
            should_resolve: true,
            regression_watch_id: None,
        };

        assert_eq!(update.source, "linear");
        assert_eq!(update.new_status, PrStatus::Merged);
        assert!(update.should_resolve);
        assert!(update.regression_watch_id.is_none());
    }

    #[test]
    fn test_github_user_serialization() {
        let user = GitHubUser {
            id: 123,
            login: "testuser".to_string(),
            user_type: Some("User".to_string()),
        };
        let json = serde_json::to_string(&user).unwrap();
        assert!(json.contains("testuser"));
        assert!(json.contains("123"));
    }

    #[test]
    fn test_pr_review_serialization() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: Some("LGTM".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: None,
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: Some("https://github.com/review".to_string()),
        };
        let json = serde_json::to_string(&review).unwrap();
        assert!(json.contains("APPROVED"));
        assert!(json.contains("LGTM"));
    }

    #[test]
    fn test_pr_review_comment_serialization() {
        let comment = PrReviewComment {
            id: 1,
            path: "src/main.rs".to_string(),
            position: Some(10),
            original_position: None,
            body: "Please fix this".to_string(),
            user: GitHubUser {
                id: 123,
                login: "commenter".to_string(),
                user_type: Some("User".to_string()),
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "https://github.com/comment".to_string(),
            pull_request_review_id: Some(5),
            start_line: None,
            line: Some(42),
            side: Some("RIGHT".to_string()),
        };
        let json = serde_json::to_string(&comment).unwrap();
        assert!(json.contains("src/main.rs"));
        assert!(json.contains("Please fix this"));
    }

    #[test]
    fn test_review_watcher_is_enabled() {
        let config_enabled = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client_enabled = GitHubClient::new(config_enabled);
        let watcher_enabled = ReviewWatcher::new(client_enabled);
        assert!(watcher_enabled.is_enabled());

        let config_disabled = GitHubConfig::default();
        let client_disabled = GitHubClient::new(config_disabled);
        let watcher_disabled = ReviewWatcher::new(client_disabled);
        assert!(!watcher_disabled.is_enabled());
    }

    #[test]
    fn test_review_event_feedback_summary_empty_body() {
        let review = PrReview {
            id: 1,
            state: "CHANGES_REQUESTED".to_string(),
            body: None,
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("CHANGES_REQUESTED"));
        assert!(!summary.contains("Review comment:"));
    }

    #[test]
    fn test_review_event_comments_multiple() {
        let comments = vec![
            PrReviewComment {
                id: 1,
                path: "file1.rs".to_string(),
                position: None,
                original_position: None,
                body: "Comment 1".to_string(),
                user: GitHubUser {
                    id: 1,
                    login: "user1".to_string(),
                    user_type: None,
                },
                created_at: String::new(),
                updated_at: String::new(),
                html_url: String::new(),
                pull_request_review_id: None,
                start_line: None,
                line: Some(10),
                side: None,
            },
            PrReviewComment {
                id: 2,
                path: "file2.rs".to_string(),
                position: None,
                original_position: None,
                body: "Comment 2".to_string(),
                user: GitHubUser {
                    id: 2,
                    login: "user2".to_string(),
                    user_type: None,
                },
                created_at: String::new(),
                updated_at: String::new(),
                html_url: String::new(),
                pull_request_review_id: None,
                start_line: None,
                line: None,
                side: None,
            },
        ];

        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments,
        };

        let summary = event.get_feedback_summary();
        assert!(summary.contains("file1.rs"));
        assert!(summary.contains("file2.rs"));
        assert!(summary.contains("line 10"));
        assert!(summary.contains("Comment 1"));
        assert!(summary.contains("Comment 2"));
    }

    #[test]
    fn test_pr_review_state_serialization() {
        let state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("url"));
        assert!(json.contains("repo"));
        assert!(json.contains("issue"));
    }

    #[test]
    fn test_pr_status_debug() {
        let open = format!("{:?}", PrStatus::Open);
        let merged = format!("{:?}", PrStatus::Merged);
        let closed = format!("{:?}", PrStatus::Closed);

        assert_eq!(open, "Open");
        assert_eq!(merged, "Merged");
        assert_eq!(closed, "Closed");
    }

    #[test]
    fn test_pr_status_copy_clone() {
        let status = PrStatus::Open;
        let copied = status;
        let cloned = status;
        assert_eq!(copied, status);
        assert_eq!(cloned, status);
    }

    #[test]
    fn test_pr_status_update_clone() {
        let update = PrStatusUpdate {
            source: "linear".to_string(),
            issue_id: "issue-1".to_string(),
            short_id: "LIN-1".to_string(),
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            new_status: PrStatus::Merged,
            should_resolve: true,
            regression_watch_id: Some(42),
        };

        let cloned = update.clone();
        assert_eq!(cloned.source, "linear");
        assert_eq!(cloned.new_status, PrStatus::Merged);
        assert_eq!(cloned.regression_watch_id, Some(42));
    }

    #[test]
    fn test_pr_status_update_debug() {
        let update = PrStatusUpdate {
            source: "sentry".to_string(),
            issue_id: "id".to_string(),
            short_id: "short".to_string(),
            pr_url: "url".to_string(),
            new_status: PrStatus::Closed,
            should_resolve: false,
            regression_watch_id: None,
        };
        let debug = format!("{:?}", update);
        assert!(debug.contains("sentry"));
        assert!(debug.contains("Closed"));
    }

    #[test]
    fn test_pr_review_state_deserialization() {
        let json = r#"{
            "pr_url": "https://github.com/test/test/pull/1",
            "repo": "test/test",
            "pr_number": 1,
            "issue_id": "issue-123",
            "source": "linear",
            "last_review_id": null,
            "last_review_time": null,
            "last_comment_id": null,
            "last_comment_time": null,
            "is_active": true
        }"#;

        let state: PrReviewState = serde_json::from_str(json).unwrap();
        assert_eq!(state.pr_url, "https://github.com/test/test/pull/1");
        assert_eq!(state.repo, "test/test");
        assert_eq!(state.pr_number, 1);
        assert!(state.is_active);
    }

    #[test]
    fn test_review_watcher_get_all_states_empty() {
        let config = GitHubConfig::default();
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        let all_states = watcher.get_all_states();
        assert!(all_states.is_empty());
    }

    #[test]
    fn test_review_watcher_overwrite_state() {
        let config = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        let state1 = PrReviewState::new("url", "repo", 1, "issue1", "linear");
        let state2 = PrReviewState::new("url", "repo", 1, "issue2", "sentry");

        watcher.watch_pr(state1);
        watcher.watch_pr(state2);

        // Second state should overwrite first (same url)
        let retrieved = watcher.get_state("url").unwrap();
        assert_eq!(retrieved.source, "sentry");
    }

    #[test]
    fn test_github_user_clone() {
        let user = GitHubUser {
            id: 123,
            login: "test".to_string(),
            user_type: Some("User".to_string()),
        };
        let cloned = user.clone();
        assert_eq!(cloned.id, 123);
        assert_eq!(cloned.login, "test");
    }

    #[test]
    fn test_pr_review_clone() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: Some("LGTM".to_string()),
            user: GitHubUser {
                id: 123,
                login: "reviewer".to_string(),
                user_type: None,
            },
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: Some("url".to_string()),
        };
        let cloned = review.clone();
        assert_eq!(cloned.id, 1);
        assert_eq!(cloned.state, "APPROVED");
    }

    #[test]
    fn test_pr_review_comment_clone() {
        let comment = PrReviewComment {
            id: 1,
            path: "file.rs".to_string(),
            position: Some(10),
            original_position: Some(5),
            body: "Comment".to_string(),
            user: GitHubUser {
                id: 1,
                login: "user".to_string(),
                user_type: None,
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "url".to_string(),
            pull_request_review_id: Some(5),
            start_line: Some(1),
            line: Some(10),
            side: Some("RIGHT".to_string()),
        };
        let cloned = comment.clone();
        assert_eq!(cloned.id, 1);
        assert_eq!(cloned.path, "file.rs");
        assert_eq!(cloned.position, Some(10));
    }

    #[test]
    fn test_review_event_clone() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: None,
            user: GitHubUser {
                id: 1,
                login: "user".to_string(),
                user_type: None,
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        let cloned = event.clone();
        assert_eq!(cloned.pr_url(), "url");
    }

    #[tokio::test]
    async fn test_review_watcher_check_for_reviews_disabled() {
        let config = GitHubConfig::default(); // No token = disabled
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        // Should return empty when disabled
        let events = watcher.check_for_reviews().await.unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_unwatch_nonexistent_pr() {
        let config = GitHubConfig {
            token: Some("test".to_string()),
            poll_interval_ms: 60000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        let watcher = ReviewWatcher::new(client);

        // Should not panic when unwatching a PR that doesn't exist
        watcher.unwatch_pr("nonexistent");

        // Verify nothing was added
        assert!(watcher.get_state("nonexistent").is_none());
    }

    #[test]
    fn test_review_event_debug() {
        let review = PrReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: None,
            user: GitHubUser {
                id: 1,
                login: "user".to_string(),
                user_type: None,
            },
            submitted_at: None,
            html_url: None,
        };

        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review,
            inline_comments: vec![],
        };

        let debug = format!("{:?}", event);
        assert!(debug.contains("ReviewSubmitted"));
    }

    #[test]
    fn test_pr_review_state_clone() {
        let state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        let cloned = state.clone();
        assert_eq!(cloned.pr_url, "url");
        assert_eq!(cloned.repo, "repo");
        assert_eq!(cloned.pr_number, 1);
    }

    #[test]
    fn test_github_client_new() {
        let config = GitHubConfig {
            token: Some("test_token".to_string()),
            poll_interval_ms: 30000,
            auto_resolve_on_merge: true,
            webhook_secret: None,
            review_trigger: "/claudear".to_string(),
            use_ssh: false,
        };
        let client = GitHubClient::new(config);
        assert!(client.is_enabled());
        assert_eq!(client.token(), Some("test_token"));
    }

    #[test]
    fn test_github_client_disabled() {
        let config = GitHubConfig::default();
        let client = GitHubClient::new(config);
        assert!(!client.is_enabled());
        assert!(client.token().is_none());
    }

    #[test]
    fn test_pull_request_deserialization() {
        let json = r#"{
            "number": 123,
            "state": "open",
            "merged": false,
            "html_url": "https://github.com/test/repo/pull/123",
            "title": "Test PR"
        }"#;
        let pr: PullRequest = serde_json::from_str(json).unwrap();
        assert_eq!(pr.state, "open");
        assert!(!pr.merged);
    }

    #[test]
    fn test_pull_request_merged() {
        let json = r#"{
            "number": 456,
            "state": "closed",
            "merged": true,
            "html_url": "https://github.com/test/repo/pull/456",
            "title": "Merged PR"
        }"#;
        let pr: PullRequest = serde_json::from_str(json).unwrap();
        assert!(pr.merged);
        assert_eq!(pr.state, "closed");
    }

    #[test]
    fn test_pr_status_update_with_regression_watch() {
        let update = PrStatusUpdate {
            source: "sentry".to_string(),
            issue_id: "issue-1".to_string(),
            short_id: "SENTRY-1".to_string(),
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            new_status: PrStatus::Merged,
            should_resolve: false, // Should be false when regression watch is active
            regression_watch_id: Some(123),
        };

        assert!(!update.should_resolve);
        assert_eq!(update.regression_watch_id, Some(123));
    }

    #[test]
    fn test_is_bug_type_sentry() {
        let mock = MockHttpClient::new();
        let config = GitHubConfig::default();
        let github_client = GitHubClient::with_http_client(config, mock);
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let monitor = PrMonitor::with_http_client(
            github_client,
            tracker.clone() as Arc<dyn FixAttemptTracker>,
            true,
        );

        // Sentry issues should always be bugs
        let sentry_attempt = FixAttempt {
            id: 1,
            issue_id: "sentry-issue-1".to_string(),
            short_id: "SENTRY-1".to_string(),
            source: "sentry".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: crate::types::FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(monitor.is_bug_type(&sentry_attempt));

        // Linear issues are not bugs by default (would need label check)
        let linear_attempt = FixAttempt {
            id: 2,
            issue_id: "linear-issue-1".to_string(),
            short_id: "LIN-1".to_string(),
            source: "linear".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: crate::types::FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(!monitor.is_bug_type(&linear_attempt));
    }

    #[test]
    fn test_get_issue_type() {
        let mock = MockHttpClient::new();
        let config = GitHubConfig::default();
        let github_client = GitHubClient::with_http_client(config, mock);
        let tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let monitor = PrMonitor::with_http_client(
            github_client,
            tracker.clone() as Arc<dyn FixAttemptTracker>,
            true,
        );

        let sentry_attempt = FixAttempt {
            id: 1,
            issue_id: "sentry-issue-1".to_string(),
            short_id: "SENTRY-1".to_string(),
            source: "sentry".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: crate::types::FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert_eq!(
            monitor.get_issue_type(&sentry_attempt),
            crate::types::IssueType::SentryIssue
        );

        let linear_attempt = FixAttempt {
            id: 2,
            issue_id: "linear-issue-1".to_string(),
            short_id: "LIN-1".to_string(),
            source: "linear".to_string(),
            attempted_at: chrono::Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: crate::types::FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert_eq!(
            monitor.get_issue_type(&linear_attempt),
            crate::types::IssueType::LinearBug
        );
    }

    #[test]
    fn test_org_repo_deserialization() {
        let json = r#"{
            "id": 123,
            "full_name": "test-org/test-repo",
            "name": "test-repo",
            "default_branch": "main",
            "clone_url": "https://github.com/test-org/test-repo.git",
            "html_url": "https://github.com/test-org/test-repo",
            "private": false,
            "archived": false
        }"#;

        let repo: OrgRepo = serde_json::from_str(json).unwrap();
        assert_eq!(repo.id, 123);
        assert_eq!(repo.full_name, "test-org/test-repo");
        assert_eq!(repo.name, "test-repo");
        assert_eq!(repo.default_branch, "main");
        assert_eq!(repo.clone_url, "https://github.com/test-org/test-repo.git");
        assert!(!repo.private);
        assert!(!repo.archived);
    }

    #[test]
    fn test_org_repo_deserialization_with_develop_branch() {
        let json = r#"{
            "id": 456,
            "full_name": "my-org/my-repo",
            "name": "my-repo",
            "default_branch": "develop",
            "clone_url": "https://github.com/my-org/my-repo.git",
            "html_url": "https://github.com/my-org/my-repo",
            "private": true,
            "archived": false
        }"#;

        let repo: OrgRepo = serde_json::from_str(json).unwrap();
        assert_eq!(repo.default_branch, "develop");
        assert!(repo.private);
    }

    #[tokio::test]
    async fn test_list_org_repos_success() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/orgs/test-org/repos?per_page=100&page=1",
            200,
            r#"[
                {
                    "id": 1,
                    "full_name": "test-org/repo1",
                    "name": "repo1",
                    "default_branch": "main",
                    "clone_url": "https://github.com/test-org/repo1.git",
                    "html_url": "https://github.com/test-org/repo1",
                    "private": false,
                    "archived": false
                },
                {
                    "id": 2,
                    "full_name": "test-org/repo2",
                    "name": "repo2",
                    "default_branch": "develop",
                    "clone_url": "https://github.com/test-org/repo2.git",
                    "html_url": "https://github.com/test-org/repo2",
                    "private": false,
                    "archived": false
                }
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test-token".to_string()),
            ..Default::default()
        };
        let client = GitHubClient::with_http_client(config, mock);

        let repos = client.list_org_repos("test-org").await.unwrap();
        assert_eq!(repos.len(), 2);
        assert_eq!(repos[0].full_name, "test-org/repo1");
        assert_eq!(repos[1].full_name, "test-org/repo2");
    }

    #[tokio::test]
    async fn test_list_org_repos_filters_archived() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://api.github.com/orgs/test-org/repos?per_page=100&page=1",
            200,
            r#"[
                {
                    "id": 1,
                    "full_name": "test-org/active-repo",
                    "name": "active-repo",
                    "default_branch": "main",
                    "clone_url": "https://github.com/test-org/active-repo.git",
                    "html_url": "https://github.com/test-org/active-repo",
                    "private": false,
                    "archived": false
                },
                {
                    "id": 2,
                    "full_name": "test-org/archived-repo",
                    "name": "archived-repo",
                    "default_branch": "main",
                    "clone_url": "https://github.com/test-org/archived-repo.git",
                    "html_url": "https://github.com/test-org/archived-repo",
                    "private": false,
                    "archived": true
                }
            ]"#,
        );

        let config = GitHubConfig {
            token: Some("test-token".to_string()),
            ..Default::default()
        };
        let client = GitHubClient::with_http_client(config, mock);

        let repos = client.list_org_repos("test-org").await.unwrap();
        // Archived repos should be filtered out
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].full_name, "test-org/active-repo");
    }

    #[tokio::test]
    async fn test_list_org_repos_org_not_found() {
        let mock = MockHttpClient::new();
        // No mock response added, so it will return 404

        let config = GitHubConfig {
            token: Some("test-token".to_string()),
            ..Default::default()
        };
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.list_org_repos("nonexistent-org").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_list_org_repos_no_token() {
        let mock = MockHttpClient::new();
        let config = GitHubConfig::default(); // No token
        let client = GitHubClient::with_http_client(config, mock);

        let result = client.list_org_repos("test-org").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("token"));
    }
}

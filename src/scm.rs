//! SCM (Source Control Management) provider abstraction.
//!
//! Defines a common trait and shared types used by both GitHub and GitLab
//! backends for PR monitoring and review watching.

use crate::error::Result;
use crate::storage::{FixAttemptTracker, SqliteTracker};
use crate::types::{ActivityLogEntry, FixAttempt, IssueType, PrReviewRecord, RegressionWatch};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ScmProvider trait

/// Trait abstracting a source-control management provider (GitHub, GitLab, ...).
///
/// Implementors must be `Send + Sync` so they can be shared across async tasks.
#[async_trait]
pub trait ScmProvider: Send + Sync {
    /// Short lowercase identifier for the provider (e.g. `"github"`, `"gitlab"`).
    fn name(&self) -> &str;

    /// Whether the provider is configured and ready to use.
    fn is_enabled(&self) -> bool;

    /// The review trigger tag (e.g. `"@claudear"`).
    fn review_trigger(&self) -> &str;

    /// Get the merge/close status of a PR/MR.
    async fn get_pr_status(&self, project: &str, number: i64) -> Result<PrStatus>;

    /// Get lightweight PR/MR info (branches, title, author).
    async fn get_pr_info(&self, project: &str, number: i64) -> Result<PrInfo>;

    /// Fetch the raw unified diff for a PR/MR.
    async fn get_pr_diff(&self, project: &str, number: i64) -> Result<String>;

    /// Get all reviews for a PR/MR.
    async fn get_reviews(&self, project: &str, number: i64) -> Result<Vec<CodeReview>>;

    /// Get all review (inline) comments for a PR/MR.
    async fn get_review_comments(&self, project: &str, number: i64) -> Result<Vec<ReviewComment>>;

    /// Get reviews submitted at or after `since` (RFC 3339 timestamp).
    ///
    /// Default implementation fetches all reviews and filters by timestamp.
    async fn get_new_reviews(
        &self,
        project: &str,
        number: i64,
        since: Option<&str>,
    ) -> Result<Vec<CodeReview>> {
        let reviews = self.get_reviews(project, number).await?;
        if let Some(since_time) = since {
            Ok(reviews
                .into_iter()
                .filter(|r| {
                    r.submitted_at
                        .as_ref()
                        .map(|t| timestamp_at_or_after(t, since_time))
                        .unwrap_or(false)
                })
                .collect())
        } else {
            Ok(reviews)
        }
    }

    /// Get review comments updated at or after `since` (RFC 3339 timestamp).
    ///
    /// Default implementation fetches all comments and filters by timestamp.
    async fn get_new_review_comments(
        &self,
        project: &str,
        number: i64,
        since: Option<&str>,
    ) -> Result<Vec<ReviewComment>> {
        let comments = self.get_review_comments(project, number).await?;
        if let Some(since_time) = since {
            Ok(comments
                .into_iter()
                .filter(|c| timestamp_at_or_after(&c.updated_at, since_time))
                .collect())
        } else {
            Ok(comments)
        }
    }

    /// List repositories for an organization / group.
    async fn list_repos(&self, org_or_group: &str) -> Result<Vec<RemoteRepo>>;

    /// Merge (squash) a PR/MR.
    async fn merge_pr(&self, _project: &str, _number: i64) -> Result<()> {
        Err(crate::error::Error::Other(
            "merge_pr not supported by this SCM provider".into(),
        ))
    }

    /// Close a PR/MR without merging.
    async fn close_pr(&self, _project: &str, _number: i64) -> Result<()> {
        Err(crate::error::Error::Other(
            "close_pr not supported by this SCM provider".into(),
        ))
    }

    /// Delete a remote branch.
    async fn delete_branch(&self, _project: &str, _branch: &str) -> Result<()> {
        Err(crate::error::Error::Other(
            "delete_branch not supported by this SCM provider".into(),
        ))
    }

    /// Post a review on a PR/MR.
    async fn post_review(
        &self,
        _project: &str,
        _number: i64,
        _action: PostReviewAction,
        _body: &str,
    ) -> Result<()> {
        Err(crate::error::Error::Other(
            "post_review not supported by this SCM provider".into(),
        ))
    }

    /// List open PRs/MRs for a project.
    async fn list_open_prs(&self, _project: &str) -> Result<Vec<PrSummary>> {
        Err(crate::error::Error::Other(
            "list_open_prs not supported by this SCM provider".into(),
        ))
    }

    /// Get the source branch name for a PR/MR.
    async fn get_pr_branch(&self, project: &str, number: i64) -> Result<String> {
        let info = self.get_pr_info(project, number).await?;
        info.head_branch.ok_or_else(|| {
            crate::error::Error::Other(format!(
                "No head branch found for PR {} in {}",
                number, project
            ))
        })
    }

    /// URL pattern for matching PR URLs in LIKE queries (e.g. `"https://github.com/%"`).
    fn pr_url_pattern(&self) -> &str {
        "%"
    }

    /// Extract a PR number from a PR URL. Returns `None` if the URL doesn't match.
    fn parse_pr_number(&self, _url: &str) -> Option<i64> {
        None
    }
}

// Shared types

/// Status of a PR / merge request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrStatus {
    Open,
    Merged,
    Closed,
}

/// Lightweight PR/MR metadata.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub head_branch: Option<String>,
    pub base_branch: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
}

/// Action to take when posting a review on a PR/MR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostReviewAction {
    /// Leave a comment without approving or requesting changes.
    Comment,
    /// Request changes before the PR can be merged.
    RequestChanges,
    /// Approve the PR.
    Approve,
}

/// Summary of an open PR/MR for listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSummary {
    pub number: i64,
    pub title: String,
    pub branch: String,
    pub url: String,
}

/// A code review (was `PrReview` in github.rs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeReview {
    /// Review ID.
    pub id: i64,
    /// Review state (APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED, PENDING).
    pub state: String,
    /// Review body/comment.
    pub body: Option<String>,
    /// Reviewer user.
    pub user: ReviewUser,
    /// When the review was submitted.
    pub submitted_at: Option<String>,
    /// HTML URL to the review.
    pub html_url: Option<String>,
}

/// A user who participated in a review (was `GitHubUser`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewUser {
    /// User ID.
    pub id: i64,
    /// Username / login.
    pub login: String,
    /// User type (User, Bot, etc.).
    #[serde(rename = "type")]
    pub user_type: Option<String>,
}

/// A repository from an organization/group listing (was `OrgRepo`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRepo {
    /// Repository ID.
    pub id: i64,
    /// Full name (org/repo or group/project).
    pub full_name: String,
    /// Repository name (without org prefix).
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

/// An inline review comment on a PR/MR (was `PrReviewComment`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
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
    pub user: ReviewUser,
    /// When the comment was created.
    pub created_at: String,
    /// When the comment was last updated.
    pub updated_at: String,
    /// HTML URL to the comment.
    pub html_url: String,
    /// Associated review ID if part of a review.
    pub pull_request_review_id: Option<i64>,
    /// Line number.
    pub line: Option<i64>,
    /// Start line (for multi-line comments).
    pub start_line: Option<i64>,
    /// Side of the diff (LEFT or RIGHT).
    pub side: Option<String>,
}

/// Persistent state for tracking review polling cursors on a single PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewState {
    /// PR URL.
    pub pr_url: String,
    /// Repository (owner/repo or group/project).
    pub repo: String,
    /// PR / MR number.
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
    /// Create a new review state with all cursors at zero.
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

/// Update information emitted when a PR's merge/close status changes.
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
    /// The issue will not be auto-resolved until regression monitoring completes.
    pub regression_watch_id: Option<i64>,
}

/// An event detected by the review watcher.
#[derive(Debug, Clone)]
pub enum ReviewEvent {
    /// A new review was submitted.
    ReviewSubmitted {
        pr_url: String,
        repo: String,
        pr_number: i64,
        review: CodeReview,
        /// Inline comments submitted as part of this review.
        inline_comments: Vec<ReviewComment>,
    },
    /// New standalone review comments were added.
    CommentsAdded {
        pr_url: String,
        repo: String,
        pr_number: i64,
        comments: Vec<ReviewComment>,
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

// Free function

/// Compare two RFC 3339 timestamps, falling back to lexicographic ordering.
pub fn compare_timestamps(a: &str, b: &str) -> std::cmp::Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(a),
        chrono::DateTime::parse_from_rfc3339(b),
    ) {
        (Ok(a_dt), Ok(b_dt)) => a_dt.cmp(&b_dt),
        _ => a.cmp(b),
    }
}

/// Returns `true` when `candidate` is at or after `since`.
///
/// Timestamps are expected in RFC 3339 format. Uses [`compare_timestamps`]
/// for timezone-safe ordering.
pub fn timestamp_at_or_after(candidate: &str, since: &str) -> bool {
    compare_timestamps(candidate, since) != std::cmp::Ordering::Less
}

// PrMonitor

/// Watches pending PRs and updates their merge/close status.
pub struct PrMonitor {
    provider: Arc<dyn ScmProvider>,
    tracker: Arc<dyn FixAttemptTracker>,
    auto_resolve: bool,
    /// Optional SQLite tracker for regression watching.
    /// When set, merged PRs for bug issues will create regression watches
    /// instead of auto-resolving.
    regression_tracker: Option<Arc<SqliteTracker>>,
}

impl PrMonitor {
    /// Create a new PR monitor.
    pub fn new(
        provider: Arc<dyn ScmProvider>,
        tracker: Arc<dyn FixAttemptTracker>,
        auto_resolve: bool,
    ) -> Self {
        Self {
            provider,
            tracker,
            auto_resolve,
            regression_tracker: None,
        }
    }

    /// Create a new PR monitor with regression tracking enabled.
    pub fn with_regression_tracking(
        provider: Arc<dyn ScmProvider>,
        tracker: Arc<dyn FixAttemptTracker>,
        auto_resolve: bool,
        regression_tracker: Arc<SqliteTracker>,
    ) -> Self {
        Self {
            provider,
            tracker,
            auto_resolve,
            regression_tracker: Some(regression_tracker),
        }
    }

    /// Determine if a fix attempt is for a bug-type issue.
    fn is_bug_type(&self, attempt: &FixAttempt) -> bool {
        attempt.is_bug()
    }

    /// Get the issue type for a fix attempt.
    fn get_issue_type(&self, attempt: &FixAttempt) -> IssueType {
        match attempt.source.as_str() {
            "sentry" => IssueType::SentryIssue,
            "linear" => IssueType::LinearBug,
            "gitlab" => IssueType::GitLabIssue,
            "jira" => IssueType::JiraIssue,
            _ => IssueType::SentryIssue,
        }
    }

    /// Check all pending PRs and update their status.
    pub async fn check_pending_prs(&self) -> Result<Vec<PrStatusUpdate>> {
        if !self.provider.is_enabled() {
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
        let repo = match &attempt.scm_repo {
            Some(r) => r,
            None => return Ok(None),
        };

        let pr_number = match attempt.scm_pr_number {
            Some(n) => n,
            None => return Ok(None),
        };

        let status = match self.provider.get_pr_status(repo, pr_number).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    source = %self.provider.name(),
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
                tracing::info!(
                    source = %self.provider.name(),
                    repo = %repo,
                    pr_number = pr_number,
                    "PR has been merged!"
                );
                self.tracker
                    .mark_merged(&attempt.source, &attempt.issue_id)?;

                // Log activity event
                let pr_url = attempt.pr_url.clone().unwrap_or_default();
                let activity = ActivityLogEntry::new("pr_merged", format!("PR merged: {}", pr_url))
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
                                    source = %self.provider.name(),
                                    issue_id = %attempt.issue_id,
                                    watch_id = watch_id,
                                    "Created regression watch for bug fix"
                                );

                                // Log activity for regression watch creation
                                let watch_activity = ActivityLogEntry::new(
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
                                    source = %self.provider.name(),
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

                // For bugs with regression tracking, don't auto-resolve yet.
                // The issue will be resolved after 24 hours of no regressions.
                let should_resolve = if regression_watch_id.is_some() {
                    false
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
                    source = %self.provider.name(),
                    repo = %repo,
                    pr_number = pr_number,
                    "PR was closed without merging"
                );
                self.tracker
                    .mark_closed(&attempt.source, &attempt.issue_id)?;

                // Log activity event
                let pr_url = attempt.pr_url.clone().unwrap_or_default();
                let activity = ActivityLogEntry::new(
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

// ReviewWatcher

/// Watches PRs for review activity (new reviews and inline comments).
pub struct ReviewWatcher {
    provider: Arc<dyn ScmProvider>,
    /// Map of PR URL -> review state.
    states: std::sync::RwLock<std::collections::HashMap<String, PrReviewState>>,
    /// Optional tracker for recording reviews to the database.
    tracker: Option<Arc<dyn FixAttemptTracker>>,
    /// Optional sqlite tracker for persisting review states.
    sqlite_tracker: Option<Arc<SqliteTracker>>,
}

impl ReviewWatcher {
    /// Create a new review watcher.
    pub fn new(provider: Arc<dyn ScmProvider>) -> Self {
        Self {
            provider,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: None,
            sqlite_tracker: None,
        }
    }

    /// Create a new review watcher with a tracker for analytics.
    pub fn with_tracker(
        provider: Arc<dyn ScmProvider>,
        tracker: Arc<dyn FixAttemptTracker>,
    ) -> Self {
        Self {
            provider,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: Some(tracker),
            sqlite_tracker: None,
        }
    }

    /// Create a new review watcher with a sqlite tracker for state persistence.
    pub fn with_sqlite_tracker(
        provider: Arc<dyn ScmProvider>,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<SqliteTracker>>,
    ) -> Self {
        Self {
            provider,
            states: std::sync::RwLock::new(std::collections::HashMap::new()),
            tracker: Some(tracker),
            sqlite_tracker,
        }
    }

    /// Check if the watcher is enabled.
    pub fn is_enabled(&self) -> bool {
        self.provider.is_enabled()
    }

    /// Start watching a PR for reviews.
    pub fn watch_pr(&self, state: PrReviewState) {
        let mut merged_state = state;
        let mut states = self.states.write().unwrap_or_else(|poisoned| {
            tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
            poisoned.into_inner()
        });

        // Preserve existing review/comment cursors when a PR is re-registered.
        // This prevents replaying the full review history on subsequent poll cycles.
        if let Some(existing) = states.get(&merged_state.pr_url) {
            merged_state.last_review_id = merged_state.last_review_id.or(existing.last_review_id);
            if merged_state.last_review_time.is_none() {
                merged_state.last_review_time = existing.last_review_time.clone();
            }
            merged_state.last_comment_id =
                merged_state.last_comment_id.or(existing.last_comment_id);
            if merged_state.last_comment_time.is_none() {
                merged_state.last_comment_time = existing.last_comment_time.clone();
            }
        }
        merged_state.is_active = true;
        states.insert(merged_state.pr_url.clone(), merged_state.clone());
        drop(states);

        // Persist merged state if sqlite_tracker is available
        if let Some(ref sqlite) = self.sqlite_tracker {
            if let Err(e) = sqlite.save_pr_review_state(&merged_state) {
                tracing::warn!(
                    component = "review_watcher",
                    pr_url = %merged_state.pr_url,
                    error = %e,
                    "Failed to persist PR review state to database"
                );
            }
        }
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
        states.remove(pr_url);
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

    /// Returns `true` when a comment is strictly after the current cursor.
    fn comment_is_after_cursor(
        comment: &ReviewComment,
        last_comment_time: Option<&str>,
        last_comment_id: Option<i64>,
    ) -> bool {
        let Some(last_time) = last_comment_time else {
            return true;
        };

        match compare_timestamps(&comment.updated_at, last_time) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => last_comment_id.map(|id| comment.id > id).unwrap_or(true),
        }
    }

    /// Check all watched PRs for new reviews.
    pub async fn check_for_reviews(&self) -> Result<Vec<ReviewEvent>> {
        if !self.provider.is_enabled() {
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

    /// Check reviews for a single watched PR (triggered by webhook).
    pub async fn check_for_pr(&self, pr_url: &str) -> Result<Vec<ReviewEvent>> {
        if !self.provider.is_enabled() {
            return Ok(vec![]);
        }

        let state = {
            let states = self.states.read().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            states.get(pr_url).filter(|s| s.is_active).cloned()
        };

        match state {
            Some(state) => self.check_pr_reviews(&state).await,
            None => Ok(vec![]),
        }
    }

    /// Record a review to the database for analytics.
    fn record_review_to_db(&self, state: &PrReviewState, review: &CodeReview) {
        if let Some(ref tracker) = self.tracker {
            let mut record = PrReviewRecord::new(&state.pr_url);
            record.reviewer = Some(review.user.login.clone());
            record.review_state = Some(review.state.clone());
            record.body = review.body.clone();

            // Parse the submitted_at timestamp
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
            let activity = ActivityLogEntry::new("pr_review_received", &message)
                .with_source(self.provider.name().to_string())
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
        let mut reviews = self
            .provider
            .get_new_reviews(
                &state.repo,
                state.pr_number,
                state.last_review_time.as_deref(),
            )
            .await?;
        reviews.sort_by_key(|r| r.id);

        // Collect review IDs we're processing this cycle so we can
        // attach their inline comments directly to the review event.
        let mut processed_review_ids: Vec<i64> = Vec::new();
        let mut latest_review_id = state.last_review_id;
        let mut latest_review_time = state.last_review_time.clone();

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

            latest_review_id = Some(latest_review_id.map_or(review.id, |id| id.max(review.id)));
            if let Some(submitted_at) = review.submitted_at.clone() {
                let should_update_time = latest_review_time
                    .as_deref()
                    .map(|existing| timestamp_at_or_after(&submitted_at, existing))
                    .unwrap_or(true);
                if should_update_time {
                    latest_review_time = Some(submitted_at);
                }
            }
        }

        if latest_review_id != state.last_review_id || latest_review_time != state.last_review_time
        {
            let mut states = self.states.write().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(s) = states.get_mut(&state.pr_url) {
                s.last_review_id = latest_review_id;
                s.last_review_time = latest_review_time.clone();

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
        let comments = match self
            .provider
            .get_new_review_comments(
                &state.repo,
                state.pr_number,
                state.last_comment_time.as_deref(),
            )
            .await
        {
            Ok(comments) => comments,
            Err(e) => {
                // Don't drop already-collected review events for this poll cycle
                // when comment retrieval fails transiently.
                tracing::warn!(
                    component = "review_watcher",
                    pr_url = %state.pr_url,
                    error = %e,
                    "Failed to fetch review comments; continuing with review events only"
                );
                return Ok(events);
            }
        };

        // Get the review trigger (e.g. "@claudear")
        let trigger = self.provider.review_trigger();

        // Cursor comments are all non-bot comments after the current cursor.
        let cursor_comments: Vec<_> = comments
            .into_iter()
            .filter(|c| c.user.user_type.as_deref() != Some("Bot"))
            .filter(|c| {
                Self::comment_is_after_cursor(
                    c,
                    state.last_comment_time.as_deref(),
                    state.last_comment_id,
                )
            })
            .collect();

        // Attach inline comments to their parent review events (these bypass
        // the trigger filter since they were submitted as part of the review).
        // Standalone comments (not part of a review we just processed) still
        // require the trigger.
        let mut attached_comment_ids: std::collections::HashSet<i64> =
            std::collections::HashSet::new();

        for comment in &cursor_comments {
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
        let standalone_comments: Vec<_> = cursor_comments
            .iter()
            .filter(|c| !attached_comment_ids.contains(&c.id))
            .filter(|c| {
                if c.pull_request_review_id.is_some() {
                    // Inline comments are review feedback and should remain actionable
                    // even when they arrive in a later poll cycle.
                    true
                } else if trigger.is_empty() {
                    true
                } else {
                    c.body.to_lowercase().contains(&trigger.to_lowercase())
                }
            })
            .cloned()
            .collect();

        if !attached_comment_ids.is_empty() || !standalone_comments.is_empty() {
            // Record all new comments to database
            if let Some(ref sqlite) = self.sqlite_tracker {
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
        }

        if !cursor_comments.is_empty() {
            // Update state cursor using all processed comments (including non-trigger comments)
            // to prevent repeatedly scanning unchanged comments every poll cycle.
            let mut latest_comment_id = state.last_comment_id;
            let mut latest_comment_time = state.last_comment_time.clone();
            for comment in &cursor_comments {
                let replace = latest_comment_time
                    .as_deref()
                    .map(|existing_time| {
                        let cmp = compare_timestamps(&comment.updated_at, existing_time);
                        cmp == std::cmp::Ordering::Greater
                            || (cmp == std::cmp::Ordering::Equal
                                && comment.id > latest_comment_id.unwrap_or(i64::MIN))
                    })
                    .unwrap_or(true);

                if replace {
                    latest_comment_id = Some(comment.id);
                    latest_comment_time = Some(comment.updated_at.clone());
                }
            }

            let mut states = self.states.write().unwrap_or_else(|poisoned| {
                tracing::warn!(component = "review_watcher", "RwLock poisoned, recovering");
                poisoned.into_inner()
            });
            if let Some(s) = states.get_mut(&state.pr_url) {
                s.last_comment_id = latest_comment_id;
                if let Some(t) = latest_comment_time {
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

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn compare_timestamps_equal() {
        let ts = "2024-01-15T12:00:00Z";
        assert_eq!(compare_timestamps(ts, ts), Ordering::Equal);
    }

    #[test]
    fn compare_timestamps_earlier_less() {
        let earlier = "2024-01-15T11:00:00Z";
        let later = "2024-01-15T12:00:00Z";
        assert_eq!(compare_timestamps(earlier, later), Ordering::Less);
    }

    #[test]
    fn compare_timestamps_later_greater() {
        let earlier = "2024-01-15T11:00:00Z";
        let later = "2024-01-15T12:00:00Z";
        assert_eq!(compare_timestamps(later, earlier), Ordering::Greater);
    }

    #[test]
    fn compare_timestamps_different_timezones_same_instant() {
        // 12:00 UTC == 06:00 -06:00
        let utc = "2024-01-01T12:00:00Z";
        let minus6 = "2024-01-01T06:00:00-06:00";
        assert_eq!(compare_timestamps(utc, minus6), Ordering::Equal);
    }

    #[test]
    fn compare_timestamps_malformed_fallback_lexicographic() {
        // Neither is valid RFC 3339 -- should fall back to string comparison.
        let a = "aaa";
        let b = "bbb";
        assert_eq!(compare_timestamps(a, b), Ordering::Less);
        assert_eq!(compare_timestamps(b, a), Ordering::Greater);
        assert_eq!(compare_timestamps(a, a), Ordering::Equal);
    }

    #[test]
    fn compare_timestamps_one_valid_one_invalid() {
        let valid = "2024-01-15T12:00:00Z";
        let invalid = "not-a-timestamp";
        // Falls back to lexicographic: '2' < 'n'
        assert_eq!(compare_timestamps(valid, invalid), Ordering::Less);
        assert_eq!(compare_timestamps(invalid, valid), Ordering::Greater);
    }

    #[test]
    fn timestamp_at_or_after_equal() {
        let ts = "2024-06-01T00:00:00Z";
        assert!(timestamp_at_or_after(ts, ts));
    }

    #[test]
    fn timestamp_at_or_after_candidate_after_since() {
        let since = "2024-06-01T00:00:00Z";
        let candidate = "2024-06-01T01:00:00Z";
        assert!(timestamp_at_or_after(candidate, since));
    }

    #[test]
    fn timestamp_at_or_after_candidate_before_since() {
        let since = "2024-06-01T01:00:00Z";
        let candidate = "2024-06-01T00:00:00Z";
        assert!(!timestamp_at_or_after(candidate, since));
    }

    #[test]
    fn timestamp_at_or_after_different_tz_same_instant() {
        // Same instant expressed in two timezones => candidate is "at" since.
        let utc = "2024-01-01T12:00:00Z";
        let plus5 = "2024-01-01T17:00:00+05:00";
        assert!(timestamp_at_or_after(utc, plus5));
        assert!(timestamp_at_or_after(plus5, utc));
    }

    #[test]
    fn timestamp_at_or_after_malformed_fallback() {
        // Both malformed -- falls back to lexicographic.
        assert!(timestamp_at_or_after("zzz", "aaa")); // "zzz" >= "aaa"
        assert!(!timestamp_at_or_after("aaa", "zzz")); // "aaa" < "zzz"
    }

    #[test]
    fn pr_status_equality() {
        assert_eq!(PrStatus::Open, PrStatus::Open);
        assert_eq!(PrStatus::Merged, PrStatus::Merged);
        assert_eq!(PrStatus::Closed, PrStatus::Closed);
    }

    #[test]
    fn pr_status_inequality() {
        assert_ne!(PrStatus::Open, PrStatus::Merged);
        assert_ne!(PrStatus::Open, PrStatus::Closed);
        assert_ne!(PrStatus::Merged, PrStatus::Closed);
    }

    #[test]
    fn pr_status_debug_format() {
        assert_eq!(format!("{:?}", PrStatus::Open), "Open");
        assert_eq!(format!("{:?}", PrStatus::Merged), "Merged");
        assert_eq!(format!("{:?}", PrStatus::Closed), "Closed");
    }

    #[test]
    fn pr_status_clone() {
        let status = PrStatus::Merged;
        let cloned = status.clone();
        assert_eq!(status, cloned);
    }

    fn make_review_user() -> ReviewUser {
        ReviewUser {
            id: 1,
            login: "reviewer".to_string(),
            user_type: Some("User".to_string()),
        }
    }

    fn make_review(state: &str, body: Option<&str>) -> CodeReview {
        CodeReview {
            id: 100,
            state: state.to_string(),
            body: body.map(|b| b.to_string()),
            user: make_review_user(),
            submitted_at: Some("2024-01-01T00:00:00Z".to_string()),
            html_url: Some("https://github.com/org/repo/pull/1#pullrequestreview-100".to_string()),
        }
    }

    fn make_review_comment(path: &str, body: &str, line: Option<i64>) -> ReviewComment {
        ReviewComment {
            id: 200,
            path: path.to_string(),
            position: None,
            original_position: None,
            body: body.to_string(),
            user: make_review_user(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "https://github.com/org/repo/pull/1#discussion_r200".to_string(),
            pull_request_review_id: None,
            line,
            start_line: None,
            side: None,
        }
    }

    #[test]
    fn review_event_pr_url_review_submitted() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", None),
            inline_comments: vec![],
        };
        assert_eq!(event.pr_url(), "https://github.com/org/repo/pull/1");
    }

    #[test]
    fn review_event_pr_url_comments_added() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "https://github.com/org/repo/pull/42".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 42,
            comments: vec![],
        };
        assert_eq!(event.pr_url(), "https://github.com/org/repo/pull/42");
    }

    #[test]
    fn review_event_requires_action_changes_requested() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", None),
            inline_comments: vec![],
        };
        assert!(event.requires_action());
    }

    #[test]
    fn review_event_requires_action_approved_false() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("APPROVED", None),
            inline_comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn review_event_requires_action_commented_true() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("COMMENTED", None),
            inline_comments: vec![],
        };
        assert!(event.requires_action());
    }

    #[test]
    fn review_event_requires_action_comments_added_nonempty() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![make_review_comment("src/main.rs", "fix this", Some(10))],
        };
        assert!(event.requires_action());
    }

    #[test]
    fn review_event_requires_action_comments_added_empty() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn review_event_feedback_summary_with_review_body() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", Some("Please fix the error handling")),
            inline_comments: vec![],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("CHANGES_REQUESTED"));
        assert!(summary.contains("Please fix the error handling"));
    }

    #[test]
    fn review_event_feedback_summary_with_empty_body() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("APPROVED", Some("")),
            inline_comments: vec![],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("APPROVED"));
        // Empty body should NOT produce "Review comment:" section
        assert!(!summary.contains("Review comment:"));
    }

    #[test]
    fn review_event_feedback_summary_with_none_body() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("APPROVED", None),
            inline_comments: vec![],
        };
        let summary = event.get_feedback_summary();
        assert!(!summary.contains("Review comment:"));
    }

    #[test]
    fn review_event_feedback_summary_with_inline_comments() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", None),
            inline_comments: vec![
                make_review_comment("src/lib.rs", "use unwrap_or_default here", Some(42)),
                make_review_comment("src/main.rs", "missing semicolon", None),
            ],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("Inline comments (2):"));
        assert!(summary.contains("`src/lib.rs`"));
        assert!(summary.contains("(line 42)"));
        assert!(summary.contains("use unwrap_or_default here"));
        assert!(summary.contains("`src/main.rs`"));
        assert!(summary.contains("missing semicolon"));
    }

    #[test]
    fn review_event_comments_summary_with_comments() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![
                make_review_comment("src/api.rs", "add validation", Some(10)),
                make_review_comment("src/db.rs", "potential SQL injection", None),
            ],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("`src/api.rs`"));
        assert!(summary.contains("(line 10)"));
        assert!(summary.contains("add validation"));
        assert!(summary.contains("`src/db.rs`"));
        assert!(summary.contains("potential SQL injection"));
        // Comment without a line number should not show "(line ...)"
        let db_section_start = summary.find("`src/db.rs`").unwrap();
        let db_section = &summary[db_section_start..];
        let next_colon = db_section.find(':').unwrap();
        let db_header = &db_section[..next_colon];
        assert!(!db_header.contains("(line"));
    }

    #[test]
    fn review_event_comments_summary_empty() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.is_empty());
    }

    #[test]
    fn pr_review_state_new_creates_correct_initial_state() {
        let state = PrReviewState::new(
            "https://github.com/org/repo/pull/5",
            "org/repo",
            5,
            "ISSUE-123",
            "linear",
        );
        assert_eq!(state.pr_url, "https://github.com/org/repo/pull/5");
        assert_eq!(state.repo, "org/repo");
        assert_eq!(state.pr_number, 5);
        assert_eq!(state.issue_id, "ISSUE-123");
        assert_eq!(state.source, "linear");
        assert!(state.last_review_id.is_none());
        assert!(state.last_review_time.is_none());
        assert!(state.last_comment_id.is_none());
        assert!(state.last_comment_time.is_none());
        assert!(state.is_active);
    }

    #[test]
    fn pr_review_state_serialization_round_trip() {
        let state = PrReviewState {
            pr_url: "https://github.com/org/repo/pull/7".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 7,
            issue_id: "LIN-42".to_string(),
            source: "linear".to_string(),
            last_review_id: Some(999),
            last_review_time: Some("2024-03-15T10:30:00Z".to_string()),
            last_comment_id: Some(888),
            last_comment_time: Some("2024-03-15T11:00:00Z".to_string()),
            is_active: true,
        };
        let json = serde_json::to_string(&state).expect("serialize");
        let deserialized: PrReviewState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(deserialized.pr_url, state.pr_url);
        assert_eq!(deserialized.repo, state.repo);
        assert_eq!(deserialized.pr_number, state.pr_number);
        assert_eq!(deserialized.issue_id, state.issue_id);
        assert_eq!(deserialized.source, state.source);
        assert_eq!(deserialized.last_review_id, state.last_review_id);
        assert_eq!(deserialized.last_review_time, state.last_review_time);
        assert_eq!(deserialized.last_comment_id, state.last_comment_id);
        assert_eq!(deserialized.last_comment_time, state.last_comment_time);
        assert_eq!(deserialized.is_active, state.is_active);
    }

    #[test]
    fn pr_review_state_update_review_tracking() {
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
    fn pr_review_state_update_comment_tracking() {
        let mut state = PrReviewState::new("url", "repo", 1, "issue", "linear");
        assert!(state.last_comment_id.is_none());
        assert!(state.last_comment_time.is_none());

        state.last_comment_id = Some(456);
        state.last_comment_time = Some("2024-01-01T01:00:00Z".to_string());

        assert_eq!(state.last_comment_id, Some(456));
        assert_eq!(
            state.last_comment_time,
            Some("2024-01-01T01:00:00Z".to_string())
        );
    }

    #[test]
    fn backward_compat_pr_review_alias() {
        let _: PrReview = CodeReview {
            id: 1,
            state: "APPROVED".to_string(),
            body: None,
            user: ReviewUser {
                id: 1,
                login: "user".to_string(),
                user_type: None,
            },
            submitted_at: None,
            html_url: None,
        };
    }

    #[test]
    fn backward_compat_pr_review_comment_alias() {
        let _: PrReviewComment = ReviewComment {
            id: 1,
            path: "file.rs".to_string(),
            position: None,
            original_position: None,
            body: "comment".to_string(),
            user: ReviewUser {
                id: 1,
                login: "user".to_string(),
                user_type: None,
            },
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            html_url: "https://example.com".to_string(),
            pull_request_review_id: None,
            line: None,
            start_line: None,
            side: None,
        };
    }

    #[test]
    fn backward_compat_github_user_alias() {
        let _: GitHubUser = ReviewUser {
            id: 1,
            login: "user".to_string(),
            user_type: Some("User".to_string()),
        };
    }

    #[test]
    fn backward_compat_org_repo_alias() {
        let _: OrgRepo = RemoteRepo {
            id: 1,
            full_name: "org/repo".to_string(),
            name: "repo".to_string(),
            default_branch: "main".to_string(),
            clone_url: "https://github.com/org/repo.git".to_string(),
            ssh_url: "git@github.com:org/repo.git".to_string(),
            html_url: "https://github.com/org/repo".to_string(),
            private: false,
            archived: false,
        };
    }

    mod trait_object_tests {
        use crate::error::Result;
        use crate::scm::{
            CodeReview, PrInfo, PrStatus, RemoteRepo, ReviewComment, ReviewUser, ScmProvider,
        };
        use async_trait::async_trait;
        use std::sync::Arc;

        struct MockScmProvider {
            name: String,
            enabled: bool,
            trigger: String,
            reviews: Vec<CodeReview>,
            comments: Vec<ReviewComment>,
        }

        impl MockScmProvider {
            fn new(name: &str, enabled: bool, trigger: &str) -> Self {
                Self {
                    name: name.to_string(),
                    enabled,
                    trigger: trigger.to_string(),
                    reviews: Vec::new(),
                    comments: Vec::new(),
                }
            }

            fn with_reviews(mut self, reviews: Vec<CodeReview>) -> Self {
                self.reviews = reviews;
                self
            }

            fn with_comments(mut self, comments: Vec<ReviewComment>) -> Self {
                self.comments = comments;
                self
            }
        }

        #[async_trait]
        impl ScmProvider for MockScmProvider {
            fn name(&self) -> &str {
                &self.name
            }

            fn is_enabled(&self) -> bool {
                self.enabled
            }

            fn review_trigger(&self) -> &str {
                &self.trigger
            }

            async fn get_pr_status(&self, _project: &str, _number: i64) -> Result<PrStatus> {
                Ok(PrStatus::Open)
            }

            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: Some("feature".to_string()),
                    base_branch: Some("main".to_string()),
                    title: Some("Mock PR".to_string()),
                    author: Some("mock-user".to_string()),
                })
            }

            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok("mock diff".to_string())
            }

            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(self.reviews.clone())
            }

            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                Ok(self.comments.clone())
            }

            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        fn make_review(id: i64, submitted_at: Option<&str>) -> CodeReview {
            CodeReview {
                id,
                state: "COMMENTED".to_string(),
                body: Some(format!("Review body {}", id)),
                user: ReviewUser {
                    id: 1,
                    login: "reviewer".to_string(),
                    user_type: Some("User".to_string()),
                },
                submitted_at: submitted_at.map(|s| s.to_string()),
                html_url: Some(format!("https://github.com/test/pr/reviews/{}", id)),
            }
        }

        fn make_comment(id: i64, updated_at: &str) -> ReviewComment {
            ReviewComment {
                id,
                path: format!("src/file_{}.rs", id),
                position: Some(10),
                original_position: Some(10),
                body: format!("Comment body {}", id),
                user: ReviewUser {
                    id: 2,
                    login: "commenter".to_string(),
                    user_type: Some("User".to_string()),
                },
                created_at: updated_at.to_string(),
                updated_at: updated_at.to_string(),
                html_url: format!("https://github.com/test/pr/comments/{}", id),
                pull_request_review_id: None,
                line: Some(10),
                start_line: None,
                side: Some("RIGHT".to_string()),
            }
        }

        #[tokio::test]
        async fn trait_object_creation_and_dispatch() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            assert_eq!(provider.name(), "github");
            assert!(provider.is_enabled());
            assert_eq!(provider.review_trigger(), "@claudear");
        }

        #[tokio::test]
        async fn trait_object_disabled_provider() {
            let mock = MockScmProvider::new("gitlab", false, "/review");
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            assert_eq!(provider.name(), "gitlab");
            assert!(!provider.is_enabled());
            assert_eq!(provider.review_trigger(), "/review");
        }

        #[tokio::test]
        async fn get_new_reviews_filters_by_since() {
            let reviews = vec![
                make_review(1, Some("2025-01-01T00:00:00Z")),
                make_review(2, Some("2025-06-15T12:00:00Z")),
                make_review(3, Some("2025-12-31T23:59:59Z")),
            ];
            let mock = MockScmProvider::new("github", true, "@claudear").with_reviews(reviews);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_reviews("org/repo", 1, Some("2025-06-15T12:00:00Z"))
                .await
                .unwrap();

            assert_eq!(filtered.len(), 2);
            assert_eq!(filtered[0].id, 2);
            assert_eq!(filtered[1].id, 3);
        }

        #[tokio::test]
        async fn get_new_reviews_none_since_returns_all() {
            let reviews = vec![
                make_review(1, Some("2025-01-01T00:00:00Z")),
                make_review(2, Some("2025-06-15T12:00:00Z")),
                make_review(3, Some("2025-12-31T23:59:59Z")),
            ];
            let mock = MockScmProvider::new("github", true, "@claudear").with_reviews(reviews);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let all = provider.get_new_reviews("org/repo", 1, None).await.unwrap();
            assert_eq!(all.len(), 3);
        }

        #[tokio::test]
        async fn get_new_reviews_excludes_reviews_without_submitted_at() {
            let reviews = vec![
                make_review(1, Some("2025-01-01T00:00:00Z")),
                make_review(2, None),
                make_review(3, Some("2025-12-31T23:59:59Z")),
            ];
            let mock = MockScmProvider::new("github", true, "@claudear").with_reviews(reviews);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_reviews("org/repo", 1, Some("2025-01-01T00:00:00Z"))
                .await
                .unwrap();

            // Review 2 has no submitted_at so it is excluded by the filter
            assert_eq!(filtered.len(), 2);
            assert_eq!(filtered[0].id, 1);
            assert_eq!(filtered[1].id, 3);
        }

        #[tokio::test]
        async fn get_new_reviews_future_since_returns_empty() {
            let reviews = vec![
                make_review(1, Some("2025-01-01T00:00:00Z")),
                make_review(2, Some("2025-06-15T12:00:00Z")),
            ];
            let mock = MockScmProvider::new("github", true, "@claudear").with_reviews(reviews);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_reviews("org/repo", 1, Some("2099-01-01T00:00:00Z"))
                .await
                .unwrap();

            assert!(filtered.is_empty());
        }

        #[tokio::test]
        async fn get_new_review_comments_filters_by_since() {
            let comments = vec![
                make_comment(10, "2025-03-01T10:00:00Z"),
                make_comment(20, "2025-06-15T12:00:00Z"),
                make_comment(30, "2025-09-20T18:30:00Z"),
            ];
            let mock = MockScmProvider::new("gitlab", true, "/review").with_comments(comments);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_review_comments("group/project", 42, Some("2025-06-15T12:00:00Z"))
                .await
                .unwrap();

            assert_eq!(filtered.len(), 2);
            assert_eq!(filtered[0].id, 20);
            assert_eq!(filtered[1].id, 30);
        }

        #[tokio::test]
        async fn get_new_review_comments_none_since_returns_all() {
            let comments = vec![
                make_comment(10, "2025-03-01T10:00:00Z"),
                make_comment(20, "2025-06-15T12:00:00Z"),
                make_comment(30, "2025-09-20T18:30:00Z"),
            ];
            let mock = MockScmProvider::new("gitlab", true, "/review").with_comments(comments);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let all = provider
                .get_new_review_comments("group/project", 42, None)
                .await
                .unwrap();

            assert_eq!(all.len(), 3);
        }

        #[tokio::test]
        async fn get_new_review_comments_future_since_returns_empty() {
            let comments = vec![
                make_comment(10, "2025-03-01T10:00:00Z"),
                make_comment(20, "2025-06-15T12:00:00Z"),
            ];
            let mock = MockScmProvider::new("gitlab", true, "/review").with_comments(comments);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_review_comments("group/project", 42, Some("2099-01-01T00:00:00Z"))
                .await
                .unwrap();

            assert!(filtered.is_empty());
        }

        #[tokio::test]
        async fn provider_interchangeability() {
            let github = MockScmProvider::new("github", true, "@claudear")
                .with_reviews(vec![make_review(1, Some("2025-01-01T00:00:00Z"))]);
            let gitlab = MockScmProvider::new("gitlab", true, "/review")
                .with_comments(vec![make_comment(10, "2025-06-01T00:00:00Z")]);

            let providers: Vec<Arc<dyn ScmProvider>> = vec![Arc::new(github), Arc::new(gitlab)];

            // Verify each returns its own name
            assert_eq!(providers[0].name(), "github");
            assert_eq!(providers[1].name(), "gitlab");

            // Call the same trait methods on each
            for provider in &providers {
                assert!(provider.is_enabled());

                let status = provider.get_pr_status("any/repo", 1).await.unwrap();
                assert_eq!(status, PrStatus::Open);

                let info = provider.get_pr_info("any/repo", 1).await.unwrap();
                assert_eq!(info.title.as_deref(), Some("Mock PR"));

                let diff = provider.get_pr_diff("any/repo", 1).await.unwrap();
                assert_eq!(diff, "mock diff");

                let repos = provider.list_repos("org").await.unwrap();
                assert!(repos.is_empty());
            }

            // Verify provider-specific data via get_new_reviews / get_new_review_comments
            let github_reviews = providers[0]
                .get_new_reviews("org/repo", 1, None)
                .await
                .unwrap();
            assert_eq!(github_reviews.len(), 1);
            assert_eq!(github_reviews[0].id, 1);

            let gitlab_comments = providers[1]
                .get_new_review_comments("group/project", 42, None)
                .await
                .unwrap();
            assert_eq!(gitlab_comments.len(), 1);
            assert_eq!(gitlab_comments[0].id, 10);
        }

        #[tokio::test]
        async fn provider_vec_iteration() {
            let names = ["github", "gitlab"];
            let providers: Vec<Arc<dyn ScmProvider>> = names
                .iter()
                .map(|n| {
                    Arc::new(MockScmProvider::new(n, true, "/trigger")) as Arc<dyn ScmProvider>
                })
                .collect();

            let collected_names: Vec<&str> = providers.iter().map(|p| p.name()).collect();
            assert_eq!(collected_names, vec!["github", "gitlab"]);
        }

        #[tokio::test]
        async fn trait_object_with_empty_reviews_and_comments() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let reviews = provider.get_new_reviews("org/repo", 1, None).await.unwrap();
            assert!(reviews.is_empty());

            let comments = provider
                .get_new_review_comments("org/repo", 1, None)
                .await
                .unwrap();
            assert!(comments.is_empty());
        }

        #[tokio::test]
        async fn get_new_reviews_exact_boundary_is_inclusive() {
            let reviews = vec![make_review(1, Some("2025-06-15T12:00:00Z"))];
            let mock = MockScmProvider::new("github", true, "@claudear").with_reviews(reviews);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            // "at or after" means the exact timestamp should be included
            let filtered = provider
                .get_new_reviews("org/repo", 1, Some("2025-06-15T12:00:00Z"))
                .await
                .unwrap();

            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered[0].id, 1);
        }

        #[tokio::test]
        async fn get_new_review_comments_exact_boundary_is_inclusive() {
            let comments = vec![make_comment(10, "2025-06-15T12:00:00Z")];
            let mock = MockScmProvider::new("gitlab", true, "/review").with_comments(comments);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);

            let filtered = provider
                .get_new_review_comments("group/project", 42, Some("2025-06-15T12:00:00Z"))
                .await
                .unwrap();

            assert_eq!(filtered.len(), 1);
            assert_eq!(filtered[0].id, 10);
        }
    }

    mod pr_monitor_tests {
        use super::*;
        use crate::error::{Error, Result};
        use crate::storage::FixAttemptTracker;
        use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus, IssueType};
        use async_trait::async_trait;
        use chrono::Utc;
        use std::collections::{HashMap, HashSet};
        use std::sync::{Arc, Mutex};

        /// A mock SCM provider that returns configurable PrStatus per (repo, pr_number).
        struct MockPrScmProvider {
            enabled: bool,
            statuses: Arc<Mutex<HashMap<(String, i64), Result<PrStatus>>>>,
        }

        impl MockPrScmProvider {
            fn new(enabled: bool) -> Self {
                Self {
                    enabled,
                    statuses: Arc::new(Mutex::new(HashMap::new())),
                }
            }

            fn with_status(self, repo: &str, pr_number: i64, status: PrStatus) -> Self {
                self.statuses
                    .lock()
                    .unwrap()
                    .insert((repo.to_string(), pr_number), Ok(status));
                self
            }

            fn with_error(self, repo: &str, pr_number: i64) -> Self {
                self.statuses.lock().unwrap().insert(
                    (repo.to_string(), pr_number),
                    Err(Error::Config("mock API error".to_string())),
                );
                self
            }
        }

        #[async_trait]
        impl ScmProvider for MockPrScmProvider {
            fn name(&self) -> &str {
                "mock"
            }

            fn is_enabled(&self) -> bool {
                self.enabled
            }

            fn review_trigger(&self) -> &str {
                "/mock"
            }

            async fn get_pr_status(&self, project: &str, number: i64) -> Result<PrStatus> {
                let map = self.statuses.lock().unwrap();
                match map.get(&(project.to_string(), number)) {
                    Some(Ok(status)) => Ok(*status),
                    Some(Err(_)) => Err(Error::Config("mock API error".to_string())),
                    None => Ok(PrStatus::Open),
                }
            }

            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: None,
                    base_branch: None,
                    title: None,
                    author: None,
                })
            }

            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok(String::new())
            }

            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(vec![])
            }

            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                Ok(vec![])
            }

            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        struct MockFixAttemptTracker {
            pending_prs: Arc<Mutex<Vec<FixAttempt>>>,
            mark_merged_calls: Arc<Mutex<Vec<(String, String)>>>,
            mark_closed_calls: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl MockFixAttemptTracker {
            fn new(pending_prs: Vec<FixAttempt>) -> Self {
                Self {
                    pending_prs: Arc::new(Mutex::new(pending_prs)),
                    mark_merged_calls: Arc::new(Mutex::new(Vec::new())),
                    mark_closed_calls: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl FixAttemptTracker for MockFixAttemptTracker {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn has_attempted(&self, _source: &str, _issue_id: &str) -> Result<bool> {
                Ok(false)
            }

            fn get_attempted_issue_ids(&self, _source: &str) -> HashSet<String> {
                HashSet::new()
            }

            fn record_attempt(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
            ) -> Result<()> {
                Ok(())
            }

            fn record_attempt_with_labels(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
                _labels: &[String],
            ) -> Result<()> {
                Ok(())
            }

            fn mark_success(&self, _source: &str, _issue_id: &str, _pr_url: &str) -> Result<()> {
                Ok(())
            }

            fn mark_failed(
                &self,
                _source: &str,
                _issue_id: &str,
                _error_message: &str,
            ) -> Result<()> {
                Ok(())
            }

            fn mark_merged(&self, source: &str, issue_id: &str) -> Result<()> {
                self.mark_merged_calls
                    .lock()
                    .unwrap()
                    .push((source.to_string(), issue_id.to_string()));
                Ok(())
            }

            fn mark_closed(&self, source: &str, issue_id: &str) -> Result<()> {
                self.mark_closed_calls
                    .lock()
                    .unwrap()
                    .push((source.to_string(), issue_id.to_string()));
                Ok(())
            }

            fn mark_resolved(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }

            fn get_attempt(&self, _source: &str, _issue_id: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }

            fn get_attempts_by_status(&self, _status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }

            fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
                Ok(self.pending_prs.lock().unwrap().clone())
            }

            fn get_attempt_by_pr_url(&self, _pr_url: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }

            fn reset_attempt(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }

            fn get_stats(&self) -> Result<FixAttemptStats> {
                Ok(FixAttemptStats {
                    total: 0,
                    pending: 0,
                    success: 0,
                    failed: 0,
                    merged: 0,
                    closed: 0,
                    cannot_fix: 0,
                    by_source: Default::default(),
                })
            }

            fn increment_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }

            fn mark_cannot_fix(&self, _source: &str, _issue_id: &str, _reason: &str) -> Result<()> {
                Ok(())
            }

            fn get_retryable_issues(&self, _max_retries: u32) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }

            fn prepare_for_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
        }

        fn make_fix_attempt(
            source: &str,
            issue_id: &str,
            short_id: &str,
            repo: Option<&str>,
            pr_number: Option<i64>,
            pr_url: Option<&str>,
            labels: Vec<String>,
        ) -> FixAttempt {
            FixAttempt {
                id: 1,
                issue_id: issue_id.to_string(),
                short_id: short_id.to_string(),
                source: source.to_string(),
                attempted_at: Utc::now(),
                pr_url: pr_url.map(|u| u.to_string()),
                scm_repo: repo.map(|r| r.to_string()),
                scm_pr_number: pr_number,
                status: FixAttemptStatus::Success,
                error_message: None,
                merged_at: None,
                resolved_at: None,
                retry_count: 0,
                last_retry_at: None,
                issue_labels: labels,
                parent_attempt_id: None,
                cascade_repo: None,
            }
        }

        #[tokio::test]
        async fn test_check_pending_prs_disabled_provider() {
            let provider = Arc::new(MockPrScmProvider::new(false));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![make_fix_attempt(
                "sentry",
                "issue-1",
                "ISSUE-1",
                Some("org/repo"),
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec![],
            )]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let updates = monitor.check_pending_prs().await.unwrap();
            assert!(updates.is_empty());
        }

        #[tokio::test]
        async fn test_check_pr_no_repo() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "sentry",
                "issue-1",
                "ISSUE-1",
                None, // no repo
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn test_check_pr_no_pr_number() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "sentry",
                "issue-1",
                "ISSUE-1",
                Some("org/repo"),
                None, // no PR number
                Some("https://github.com/org/repo/pull/42"),
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn test_check_pr_merged_auto_resolve() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Merged,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker.clone(), true);

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                Some("org/repo"),
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec![], // no bug labels, and source is "linear" so is_bug() is false
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.new_status, PrStatus::Merged);
            assert!(result.should_resolve);
            assert!(result.regression_watch_id.is_none());
            assert_eq!(result.source, "linear");
            assert_eq!(result.issue_id, "issue-1");
            assert_eq!(result.short_id, "LIN-1");

            // Verify mark_merged was called
            let merged_calls = tracker.mark_merged_calls.lock().unwrap();
            assert_eq!(merged_calls.len(), 1);
            assert_eq!(
                merged_calls[0],
                ("linear".to_string(), "issue-1".to_string())
            );
        }

        #[tokio::test]
        async fn test_check_pr_merged_no_auto_resolve() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Merged,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker.clone(), false); // auto_resolve = false

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                Some("org/repo"),
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.new_status, PrStatus::Merged);
            assert!(!result.should_resolve);
        }

        #[tokio::test]
        async fn test_check_pr_closed() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                10,
                PrStatus::Closed,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker.clone(), true);

            let attempt = make_fix_attempt(
                "sentry",
                "issue-2",
                "SENTRY-2",
                Some("org/repo"),
                Some(10),
                Some("https://github.com/org/repo/pull/10"),
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.new_status, PrStatus::Closed);
            assert!(!result.should_resolve);
            assert!(result.regression_watch_id.is_none());

            // Verify mark_closed was called
            let closed_calls = tracker.mark_closed_calls.lock().unwrap();
            assert_eq!(closed_calls.len(), 1);
            assert_eq!(
                closed_calls[0],
                ("sentry".to_string(), "issue-2".to_string())
            );
        }

        #[tokio::test]
        async fn test_check_pr_still_open() {
            let provider =
                Arc::new(MockPrScmProvider::new(true).with_status("org/repo", 5, PrStatus::Open));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "sentry",
                "issue-3",
                "SENTRY-3",
                Some("org/repo"),
                Some(5),
                None,
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn test_check_pr_api_error() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_error("org/repo", 99));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "sentry",
                "issue-4",
                "SENTRY-4",
                Some("org/repo"),
                Some(99),
                Some("https://github.com/org/repo/pull/99"),
                vec![],
            );

            // Should not panic, should return None
            let result = monitor.check_pr(&attempt).await.unwrap();
            assert!(result.is_none());
        }

        #[tokio::test]
        async fn test_check_pending_prs_multiple() {
            let provider = Arc::new(
                MockPrScmProvider::new(true)
                    .with_status("org/repo", 1, PrStatus::Merged)
                    .with_status("org/repo", 2, PrStatus::Closed)
                    .with_status("org/repo", 3, PrStatus::Open),
            );

            let attempts = vec![
                make_fix_attempt(
                    "linear",
                    "issue-a",
                    "LIN-A",
                    Some("org/repo"),
                    Some(1),
                    Some("https://github.com/org/repo/pull/1"),
                    vec![],
                ),
                make_fix_attempt(
                    "sentry",
                    "issue-b",
                    "SENTRY-B",
                    Some("org/repo"),
                    Some(2),
                    Some("https://github.com/org/repo/pull/2"),
                    vec![],
                ),
                make_fix_attempt(
                    "sentry",
                    "issue-c",
                    "SENTRY-C",
                    Some("org/repo"),
                    Some(3),
                    Some("https://github.com/org/repo/pull/3"),
                    vec![],
                ),
            ];

            let tracker = Arc::new(MockFixAttemptTracker::new(attempts));
            let monitor = PrMonitor::new(provider, tracker.clone(), true);

            let updates = monitor.check_pending_prs().await.unwrap();

            // Open PR returns None, so only 2 updates (merged + closed)
            assert_eq!(updates.len(), 2);

            let merged = updates
                .iter()
                .find(|u| u.new_status == PrStatus::Merged)
                .unwrap();
            assert_eq!(merged.issue_id, "issue-a");
            assert_eq!(merged.source, "linear");

            let closed = updates
                .iter()
                .find(|u| u.new_status == PrStatus::Closed)
                .unwrap();
            assert_eq!(closed.issue_id, "issue-b");
            assert_eq!(closed.source, "sentry");
            assert!(!closed.should_resolve);

            // Verify tracker calls
            let merged_calls = tracker.mark_merged_calls.lock().unwrap();
            assert_eq!(merged_calls.len(), 1);
            assert_eq!(merged_calls[0].1, "issue-a");

            let closed_calls = tracker.mark_closed_calls.lock().unwrap();
            assert_eq!(closed_calls.len(), 1);
            assert_eq!(closed_calls[0].1, "issue-b");
        }

        #[tokio::test]
        async fn test_get_issue_type_sentry() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt =
                make_fix_attempt("sentry", "issue-1", "SENTRY-1", None, None, None, vec![]);

            assert_eq!(monitor.get_issue_type(&attempt), IssueType::SentryIssue);
        }

        #[tokio::test]
        async fn test_get_issue_type_linear() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt("linear", "issue-1", "LIN-1", None, None, None, vec![]);

            assert_eq!(monitor.get_issue_type(&attempt), IssueType::LinearBug);
        }

        #[tokio::test]
        async fn test_get_issue_type_gitlab() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt("gitlab", "issue-1", "GL-1", None, None, None, vec![]);

            assert_eq!(monitor.get_issue_type(&attempt), IssueType::GitLabIssue);
        }

        #[tokio::test]
        async fn test_get_issue_type_jira() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt("jira", "issue-1", "JIRA-1", None, None, None, vec![]);

            assert_eq!(monitor.get_issue_type(&attempt), IssueType::JiraIssue);
        }

        #[tokio::test]
        async fn test_get_issue_type_unknown() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt("unknown", "issue-1", "UNK-1", None, None, None, vec![]);

            // Unknown source defaults to SentryIssue
            assert_eq!(monitor.get_issue_type(&attempt), IssueType::SentryIssue);
        }
    }

    mod review_watcher_tests {
        use crate::error::Result;
        use crate::scm::{
            CodeReview, PrInfo, PrReviewState, PrStatus, RemoteRepo, ReviewComment, ReviewEvent,
            ReviewUser, ReviewWatcher, ScmProvider,
        };
        use async_trait::async_trait;
        use std::sync::{Arc, Mutex};

        struct MockScmProvider {
            name: String,
            enabled: bool,
            trigger: String,
            reviews: Arc<Mutex<Vec<CodeReview>>>,
            comments: Arc<Mutex<Vec<ReviewComment>>>,
            /// When true, get_review_comments returns an error.
            comments_error: Arc<Mutex<bool>>,
        }

        impl MockScmProvider {
            fn new(name: &str, enabled: bool, trigger: &str) -> Self {
                Self {
                    name: name.to_string(),
                    enabled,
                    trigger: trigger.to_string(),
                    reviews: Arc::new(Mutex::new(Vec::new())),
                    comments: Arc::new(Mutex::new(Vec::new())),
                    comments_error: Arc::new(Mutex::new(false)),
                }
            }

            fn set_reviews(&self, reviews: Vec<CodeReview>) {
                *self.reviews.lock().unwrap() = reviews;
            }

            fn set_comments(&self, comments: Vec<ReviewComment>) {
                *self.comments.lock().unwrap() = comments;
            }

            fn set_comments_error(&self, should_error: bool) {
                *self.comments_error.lock().unwrap() = should_error;
            }
        }

        #[async_trait]
        impl ScmProvider for MockScmProvider {
            fn name(&self) -> &str {
                &self.name
            }

            fn is_enabled(&self) -> bool {
                self.enabled
            }

            fn review_trigger(&self) -> &str {
                &self.trigger
            }

            async fn get_pr_status(&self, _project: &str, _number: i64) -> Result<PrStatus> {
                Ok(PrStatus::Open)
            }

            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: Some("feature".to_string()),
                    base_branch: Some("main".to_string()),
                    title: Some("Mock PR".to_string()),
                    author: Some("mock-user".to_string()),
                })
            }

            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok("mock diff".to_string())
            }

            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(self.reviews.lock().unwrap().clone())
            }

            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                if *self.comments_error.lock().unwrap() {
                    return Err(crate::error::Error::Source {
                        source_name: "mock".to_string(),
                        message: "simulated comment fetch error".to_string(),
                    });
                }
                Ok(self.comments.lock().unwrap().clone())
            }

            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        fn make_review(id: i64, state: &str, user: &str, submitted_at: &str) -> CodeReview {
            CodeReview {
                id,
                state: state.to_string(),
                body: Some(format!("Review body {}", id)),
                user: ReviewUser {
                    id: id + 1000,
                    login: user.to_string(),
                    user_type: Some("User".to_string()),
                },
                submitted_at: Some(submitted_at.to_string()),
                html_url: Some(format!("https://github.com/org/repo/pull/1#review-{}", id)),
            }
        }

        fn make_bot_review(id: i64, state: &str, submitted_at: &str) -> CodeReview {
            CodeReview {
                id,
                state: state.to_string(),
                body: Some("Bot review".to_string()),
                user: ReviewUser {
                    id: id + 1000,
                    login: "dependabot[bot]".to_string(),
                    user_type: Some("Bot".to_string()),
                },
                submitted_at: Some(submitted_at.to_string()),
                html_url: None,
            }
        }

        fn make_comment(
            id: i64,
            body: &str,
            updated_at: &str,
            pull_request_review_id: Option<i64>,
        ) -> ReviewComment {
            ReviewComment {
                id,
                path: format!("src/file_{}.rs", id),
                position: Some(10),
                original_position: Some(10),
                body: body.to_string(),
                user: ReviewUser {
                    id: id + 2000,
                    login: "commenter".to_string(),
                    user_type: Some("User".to_string()),
                },
                created_at: updated_at.to_string(),
                updated_at: updated_at.to_string(),
                html_url: format!("https://github.com/org/repo/pull/1#comment-{}", id),
                pull_request_review_id,
                line: Some(10),
                start_line: None,
                side: Some("RIGHT".to_string()),
            }
        }

        fn make_state(pr_url: &str, repo: &str, pr_number: i64) -> PrReviewState {
            PrReviewState::new(pr_url, repo, pr_number, "ISSUE-1", "linear")
        }

        #[test]
        fn test_watch_pr_adds_state() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            let state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            watcher.watch_pr(state);

            let retrieved = watcher.get_state("https://github.com/org/repo/pull/1");
            assert!(retrieved.is_some());
            let s = retrieved.unwrap();
            assert_eq!(s.pr_url, "https://github.com/org/repo/pull/1");
            assert_eq!(s.repo, "org/repo");
            assert_eq!(s.pr_number, 1);
            assert!(s.is_active);
        }

        #[test]
        fn test_unwatch_pr_removes() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            let state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            watcher.watch_pr(state);
            watcher.unwatch_pr("https://github.com/org/repo/pull/1");

            let retrieved = watcher.get_state("https://github.com/org/repo/pull/1");
            assert!(retrieved.is_none());
        }

        #[test]
        fn test_get_active_states_filters_inactive() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));
            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/2",
                "org/repo",
                2,
            ));
            watcher.unwatch_pr("https://github.com/org/repo/pull/1");

            let active = watcher.get_active_states();
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].pr_url, "https://github.com/org/repo/pull/2");
        }

        #[test]
        fn test_get_all_states_excludes_unwatched() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));
            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/2",
                "org/repo",
                2,
            ));
            watcher.unwatch_pr("https://github.com/org/repo/pull/1");

            let all = watcher.get_all_states();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].pr_url, "https://github.com/org/repo/pull/2");
        }

        #[test]
        fn test_load_states_only_active() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            let mut active_state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            active_state.is_active = true;

            let mut inactive_state =
                make_state("https://github.com/org/repo/pull/2", "org/repo", 2);
            inactive_state.is_active = false;

            watcher.load_states(vec![active_state, inactive_state]);

            let all = watcher.get_all_states();
            assert_eq!(all.len(), 1);
            assert_eq!(all[0].pr_url, "https://github.com/org/repo/pull/1");
        }

        #[test]
        fn test_watch_pr_preserves_existing_cursors() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            // First watch with cursors already set
            let mut state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            state.last_review_id = Some(42);
            state.last_review_time = Some("2025-01-01T00:00:00Z".to_string());
            state.last_comment_id = Some(99);
            state.last_comment_time = Some("2025-01-02T00:00:00Z".to_string());
            watcher.watch_pr(state);

            // Re-watch the same PR with a fresh state (no cursors)
            let fresh_state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            watcher.watch_pr(fresh_state);

            let retrieved = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(retrieved.last_review_id, Some(42));
            assert_eq!(
                retrieved.last_review_time,
                Some("2025-01-01T00:00:00Z".to_string())
            );
            assert_eq!(retrieved.last_comment_id, Some(99));
            assert_eq!(
                retrieved.last_comment_time,
                Some("2025-01-02T00:00:00Z".to_string())
            );
            assert!(retrieved.is_active);
        }

        #[test]
        fn test_is_enabled_delegates_to_provider() {
            let enabled_provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let disabled_provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", false, "@claudear"));

            let watcher_enabled = ReviewWatcher::new(enabled_provider);
            let watcher_disabled = ReviewWatcher::new(disabled_provider);

            assert!(watcher_enabled.is_enabled());
            assert!(!watcher_disabled.is_enabled());
        }

        #[test]
        fn test_comment_after_cursor_no_cursor() {
            let comment = make_comment(1, "body", "2025-01-01T00:00:00Z", None);
            assert!(ReviewWatcher::comment_is_after_cursor(&comment, None, None));
        }

        #[test]
        fn test_comment_after_cursor_newer_timestamp() {
            let comment = make_comment(1, "body", "2025-01-02T00:00:00Z", None);
            assert!(ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                Some(0),
            ));
        }

        #[test]
        fn test_comment_after_cursor_older_timestamp() {
            let comment = make_comment(1, "body", "2024-12-31T00:00:00Z", None);
            assert!(!ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                Some(0),
            ));
        }

        #[test]
        fn test_comment_after_cursor_same_timestamp_higher_id() {
            let comment = make_comment(10, "body", "2025-01-01T00:00:00Z", None);
            assert!(ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                Some(5),
            ));
        }

        #[test]
        fn test_comment_after_cursor_same_timestamp_lower_id() {
            let comment = make_comment(3, "body", "2025-01-01T00:00:00Z", None);
            assert!(!ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                Some(5),
            ));
        }

        #[test]
        fn test_comment_after_cursor_same_timestamp_no_id() {
            let comment = make_comment(1, "body", "2025-01-01T00:00:00Z", None);
            // When last_comment_id is None but timestamps are equal,
            // should return true
            assert!(ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                None,
            ));
        }

        #[tokio::test]
        async fn test_check_for_reviews_disabled() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", false, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            let events = watcher.check_for_reviews().await.unwrap();
            assert!(events.is_empty());
        }

        #[tokio::test]
        async fn test_check_for_reviews_no_active_states() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);
            // No watch_pr called, so no active states
            let events = watcher.check_for_reviews().await.unwrap();
            assert!(events.is_empty());
        }

        #[tokio::test]
        async fn test_check_for_reviews_new_review() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            assert_eq!(events.len(), 1);
            match &events[0] {
                ReviewEvent::ReviewSubmitted {
                    pr_url,
                    repo,
                    pr_number,
                    review,
                    ..
                } => {
                    assert_eq!(pr_url, "https://github.com/org/repo/pull/1");
                    assert_eq!(repo, "org/repo");
                    assert_eq!(*pr_number, 1);
                    assert_eq!(review.id, 1);
                    assert_eq!(review.state, "CHANGES_REQUESTED");
                    assert_eq!(review.user.login, "reviewer");
                }
                _ => panic!("Expected ReviewSubmitted event"),
            }
        }

        #[tokio::test]
        async fn test_check_for_reviews_skips_bot_reviews() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_bot_review(
                1,
                "COMMENTED",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            let review_submitted_count = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .count();
            assert_eq!(review_submitted_count, 0);
        }

        #[tokio::test]
        async fn test_check_for_reviews_skips_pending_reviews() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "PENDING",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            let review_submitted_count = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .count();
            assert_eq!(review_submitted_count, 0);
        }

        #[tokio::test]
        async fn test_check_for_reviews_skips_already_processed() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                5,
                "COMMENTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            // Set up state with last_review_id = 5 so review id 5
            // is already processed
            let mut state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            state.last_review_id = Some(5);
            state.last_review_time = Some("2025-01-01T00:00:00Z".to_string());
            watcher.watch_pr(state);

            let events = watcher.check_for_reviews().await.unwrap();
            let review_submitted_count = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .count();
            assert_eq!(review_submitted_count, 0);
        }

        #[tokio::test]
        async fn test_check_for_reviews_advances_cursor() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![
                make_review(1, "COMMENTED", "reviewer", "2025-01-01T00:00:00Z"),
                make_review(2, "CHANGES_REQUESTED", "reviewer", "2025-01-02T00:00:00Z"),
            ]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let _ = watcher.check_for_reviews().await.unwrap();

            // After processing, cursors should be advanced
            let state = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(state.last_review_id, Some(2));
            assert_eq!(
                state.last_review_time,
                Some("2025-01-02T00:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_check_for_pr_unwatched() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            let events = watcher
                .check_for_pr("https://github.com/org/repo/pull/999")
                .await
                .unwrap();
            assert!(events.is_empty());
        }

        #[tokio::test]
        async fn test_check_for_reviews_standalone_comment_with_trigger() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            // No reviews, just a standalone comment with the trigger
            mock.set_comments(vec![make_comment(
                1,
                "Please fix this @claudear",
                "2025-01-01T00:00:00Z",
                None,
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();

            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert_eq!(comments_events.len(), 1);
            if let ReviewEvent::CommentsAdded { comments, .. } = &comments_events[0] {
                assert_eq!(comments.len(), 1);
                assert_eq!(comments[0].id, 1);
                assert!(comments[0].body.contains("@claudear"));
            } else {
                panic!("Expected CommentsAdded");
            }
        }

        #[tokio::test]
        async fn test_check_for_reviews_standalone_comment_without_trigger() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            // Comment without the trigger keyword and no
            // pull_request_review_id
            mock.set_comments(vec![make_comment(
                1,
                "This is just a normal comment",
                "2025-01-01T00:00:00Z",
                None,
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();

            // No CommentsAdded since the standalone comment lacks the
            // trigger
            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert_eq!(comments_events.len(), 0);
        }

        #[tokio::test]
        async fn test_check_for_reviews_comment_attached_to_review() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let review_id = 10;
            mock.set_reviews(vec![make_review(
                review_id,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            mock.set_comments(vec![make_comment(
                100,
                "inline feedback",
                "2025-01-01T00:00:00Z",
                Some(review_id),
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();

            let review_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .collect();
            assert_eq!(review_events.len(), 1);
            if let ReviewEvent::ReviewSubmitted {
                review,
                inline_comments,
                ..
            } = &review_events[0]
            {
                assert_eq!(review.id, review_id);
                assert_eq!(inline_comments.len(), 1);
                assert_eq!(inline_comments[0].id, 100);
                assert_eq!(inline_comments[0].body, "inline feedback");
            } else {
                panic!("Expected ReviewSubmitted");
            }
        }

        #[tokio::test]
        async fn test_check_for_reviews_comment_fetch_error_preserves_events() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            mock.set_comments_error(true);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();

            // Review events should still be returned despite comment
            // fetch failure
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], ReviewEvent::ReviewSubmitted { .. }));
        }
    }

    // ================================================================
    // Additional coverage: type serialization, Display impls,
    // ReviewEvent edge cases, PrReviewState JSON, ReviewWatcher
    // state management, comment_is_after_cursor boundary cases,
    // and compare_timestamps with sub-second precision.
    // ================================================================

    #[test]
    fn code_review_serde_round_trip_full() {
        let review = CodeReview {
            id: 42,
            state: "APPROVED".to_string(),
            body: Some("Looks good!".to_string()),
            user: ReviewUser {
                id: 7,
                login: "octocat".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2025-03-15T10:30:00Z".to_string()),
            html_url: Some("https://github.com/org/repo/pull/1#pullrequestreview-42".to_string()),
        };

        let json = serde_json::to_string(&review).unwrap();
        let deserialized: CodeReview = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, 42);
        assert_eq!(deserialized.state, "APPROVED");
        assert_eq!(deserialized.body.as_deref(), Some("Looks good!"));
        assert_eq!(deserialized.user.id, 7);
        assert_eq!(deserialized.user.login, "octocat");
        assert_eq!(deserialized.user.user_type.as_deref(), Some("User"));
        assert_eq!(
            deserialized.submitted_at.as_deref(),
            Some("2025-03-15T10:30:00Z")
        );
        assert!(deserialized.html_url.is_some());
    }

    #[test]
    fn code_review_serde_minimal_json() {
        // Minimal JSON with only required fields; optionals missing
        let json = r#"{"id":1,"state":"COMMENTED","body":null,"user":{"id":2,"login":"dev"},"submitted_at":null,"html_url":null}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.id, 1);
        assert_eq!(review.state, "COMMENTED");
        assert!(review.body.is_none());
        assert!(review.user.user_type.is_none());
        assert!(review.submitted_at.is_none());
        assert!(review.html_url.is_none());
    }

    #[test]
    fn review_user_serde_type_field_rename() {
        let json = r#"{"id":99,"login":"bot-user","type":"Bot"}"#;
        let user: ReviewUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.id, 99);
        assert_eq!(user.login, "bot-user");
        assert_eq!(user.user_type.as_deref(), Some("Bot"));

        // Serialize back should use "type" not "user_type"
        let serialized = serde_json::to_string(&user).unwrap();
        assert!(
            serialized.contains("\"type\":"),
            "Serialized JSON should use 'type' key, got: {}",
            serialized
        );
        assert!(
            !serialized.contains("\"user_type\":"),
            "Should not contain 'user_type' key"
        );
    }

    #[test]
    fn review_user_serde_missing_type_field() {
        let json = r#"{"id":10,"login":"anon"}"#;
        let user: ReviewUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.id, 10);
        assert_eq!(user.login, "anon");
        assert!(user.user_type.is_none());
    }

    #[test]
    fn remote_repo_serde_round_trip() {
        let repo = RemoteRepo {
            id: 123,
            full_name: "org/my-repo".to_string(),
            name: "my-repo".to_string(),
            default_branch: "main".to_string(),
            clone_url: "https://github.com/org/my-repo.git".to_string(),
            ssh_url: "git@github.com:org/my-repo.git".to_string(),
            html_url: "https://github.com/org/my-repo".to_string(),
            private: true,
            archived: false,
        };

        let json = serde_json::to_string(&repo).unwrap();
        let deserialized: RemoteRepo = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, 123);
        assert_eq!(deserialized.full_name, "org/my-repo");
        assert_eq!(deserialized.name, "my-repo");
        assert_eq!(deserialized.default_branch, "main");
        assert!(deserialized.private);
        assert!(!deserialized.archived);
    }

    #[test]
    fn remote_repo_serde_ssh_url_defaults_empty() {
        // ssh_url has #[serde(default)], so missing field should default to ""
        let json = r#"{"id":1,"full_name":"o/r","name":"r","default_branch":"main","clone_url":"https://x","html_url":"https://y","private":false,"archived":false}"#;
        let repo: RemoteRepo = serde_json::from_str(json).unwrap();
        assert_eq!(repo.ssh_url, "");
    }

    #[test]
    fn review_comment_serde_round_trip() {
        let comment = ReviewComment {
            id: 500,
            path: "src/main.rs".to_string(),
            position: Some(15),
            original_position: Some(15),
            body: "Fix this line".to_string(),
            user: ReviewUser {
                id: 1,
                login: "reviewer".to_string(),
                user_type: Some("User".to_string()),
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-02T00:00:00Z".to_string(),
            html_url: "https://github.com/org/repo/pull/1#discussion_r500".to_string(),
            pull_request_review_id: Some(42),
            line: Some(15),
            start_line: Some(10),
            side: Some("RIGHT".to_string()),
        };

        let json = serde_json::to_string(&comment).unwrap();
        let deserialized: ReviewComment = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, 500);
        assert_eq!(deserialized.path, "src/main.rs");
        assert_eq!(deserialized.position, Some(15));
        assert_eq!(deserialized.pull_request_review_id, Some(42));
        assert_eq!(deserialized.line, Some(15));
        assert_eq!(deserialized.start_line, Some(10));
        assert_eq!(deserialized.side.as_deref(), Some("RIGHT"));
    }

    #[test]
    fn review_comment_serde_minimal_optional_fields() {
        let json = r#"{
            "id": 1,
            "path": "file.rs",
            "body": "comment",
            "user": {"id": 2, "login": "u"},
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "html_url": "https://example.com"
        }"#;
        let comment: ReviewComment = serde_json::from_str(json).unwrap();
        assert_eq!(comment.id, 1);
        assert!(comment.position.is_none());
        assert!(comment.original_position.is_none());
        assert!(comment.pull_request_review_id.is_none());
        assert!(comment.line.is_none());
        assert!(comment.start_line.is_none());
        assert!(comment.side.is_none());
    }

    #[test]
    fn pr_review_state_deserialize_from_json() {
        let json = r#"{
            "pr_url": "https://github.com/org/repo/pull/10",
            "repo": "org/repo",
            "pr_number": 10,
            "issue_id": "LIN-55",
            "source": "linear",
            "last_review_id": null,
            "last_review_time": null,
            "last_comment_id": null,
            "last_comment_time": null,
            "is_active": true
        }"#;
        let state: PrReviewState = serde_json::from_str(json).unwrap();
        assert_eq!(state.pr_url, "https://github.com/org/repo/pull/10");
        assert_eq!(state.pr_number, 10);
        assert_eq!(state.issue_id, "LIN-55");
        assert!(state.is_active);
        assert!(state.last_review_id.is_none());
    }

    #[test]
    fn pr_review_state_deserialize_with_all_cursors_set() {
        let json = r#"{
            "pr_url": "https://github.com/org/repo/pull/3",
            "repo": "org/repo",
            "pr_number": 3,
            "issue_id": "SENTRY-100",
            "source": "sentry",
            "last_review_id": 500,
            "last_review_time": "2025-06-01T12:00:00Z",
            "last_comment_id": 600,
            "last_comment_time": "2025-06-01T13:00:00Z",
            "is_active": false
        }"#;
        let state: PrReviewState = serde_json::from_str(json).unwrap();
        assert_eq!(state.last_review_id, Some(500));
        assert_eq!(
            state.last_review_time.as_deref(),
            Some("2025-06-01T12:00:00Z")
        );
        assert_eq!(state.last_comment_id, Some(600));
        assert!(!state.is_active);
    }

    #[test]
    fn pr_info_all_none() {
        let info = PrInfo {
            head_branch: None,
            base_branch: None,
            title: None,
            author: None,
        };
        assert!(info.head_branch.is_none());
        assert!(info.base_branch.is_none());
        assert!(info.title.is_none());
        assert!(info.author.is_none());
    }

    #[test]
    fn pr_info_debug_output() {
        let info = PrInfo {
            head_branch: Some("feature/test".to_string()),
            base_branch: Some("main".to_string()),
            title: Some("My PR".to_string()),
            author: Some("dev".to_string()),
        };
        let debug = format!("{:?}", info);
        assert!(debug.contains("feature/test"));
        assert!(debug.contains("main"));
        assert!(debug.contains("My PR"));
    }

    #[test]
    fn pr_status_update_debug_format() {
        let update = PrStatusUpdate {
            source: "linear".to_string(),
            issue_id: "LIN-99".to_string(),
            short_id: "LIN-99".to_string(),
            pr_url: "https://github.com/org/repo/pull/99".to_string(),
            new_status: PrStatus::Merged,
            should_resolve: true,
            regression_watch_id: Some(42),
        };
        let debug = format!("{:?}", update);
        assert!(debug.contains("Merged"));
        assert!(debug.contains("LIN-99"));
    }

    #[test]
    fn review_event_requires_action_dismissed() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("DISMISSED", None),
            inline_comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn review_event_requires_action_pending() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("PENDING", None),
            inline_comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn review_event_requires_action_lowercase_changes_requested() {
        // The code uppercases the state, so lowercase should still work
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("changes_requested", None),
            inline_comments: vec![],
        };
        assert!(event.requires_action());
    }

    #[test]
    fn review_event_requires_action_mixed_case_commented() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("Commented", None),
            inline_comments: vec![],
        };
        assert!(event.requires_action());
    }

    #[test]
    fn review_event_feedback_summary_review_with_inline_no_body() {
        // Review with no body but with inline comments
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", None),
            inline_comments: vec![make_review_comment("src/lib.rs", "fix this", Some(5))],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("@reviewer"));
        assert!(summary.contains("CHANGES_REQUESTED"));
        assert!(!summary.contains("Review comment:"));
        assert!(summary.contains("Inline comments (1):"));
        assert!(summary.contains("`src/lib.rs`"));
        assert!(summary.contains("(line 5)"));
    }

    #[test]
    fn review_event_feedback_summary_comments_added_no_line() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![make_review_comment("README.md", "update this", None)],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("`README.md`"));
        assert!(summary.contains("update this"));
        assert!(!summary.contains("(line"));
    }

    #[test]
    fn review_event_clone_preserves_pr_url() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "https://github.com/org/repo/pull/42".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 42,
            review: make_review("APPROVED", Some("LGTM")),
            inline_comments: vec![],
        };
        let cloned = event.clone();
        assert_eq!(cloned.pr_url(), "https://github.com/org/repo/pull/42");
    }

    #[test]
    fn compare_timestamps_subsecond_precision() {
        let a = "2025-06-15T12:00:00.100Z";
        let b = "2025-06-15T12:00:00.200Z";
        assert_eq!(compare_timestamps(a, b), Ordering::Less);
        assert_eq!(compare_timestamps(b, a), Ordering::Greater);
    }

    #[test]
    fn compare_timestamps_nanosecond_equal() {
        let ts = "2025-06-15T12:00:00.123456789Z";
        assert_eq!(compare_timestamps(ts, ts), Ordering::Equal);
    }

    #[test]
    fn timestamp_at_or_after_subsecond_candidate_after() {
        assert!(timestamp_at_or_after(
            "2025-06-15T12:00:00.500Z",
            "2025-06-15T12:00:00.499Z"
        ));
    }

    #[test]
    fn timestamp_at_or_after_subsecond_candidate_before() {
        assert!(!timestamp_at_or_after(
            "2025-06-15T12:00:00.499Z",
            "2025-06-15T12:00:00.500Z"
        ));
    }

    #[test]
    fn pr_review_state_clone_independence() {
        let state = PrReviewState::new("url", "repo", 1, "issue", "src");
        let mut cloned = state.clone();
        cloned.last_review_id = Some(99);
        cloned.is_active = false;

        // Original should be unaffected
        assert!(state.last_review_id.is_none());
        assert!(state.is_active);
    }

    #[test]
    fn pr_status_copy_semantics() {
        let status = PrStatus::Open;
        let copied = status; // Copy
        assert_eq!(status, copied); // Both still valid
        assert_eq!(status, PrStatus::Open);
    }

    #[test]
    fn review_event_debug_review_submitted() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            repo: "org/repo".to_string(),
            pr_number: 1,
            review: make_review("APPROVED", None),
            inline_comments: vec![],
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("ReviewSubmitted"));
    }

    #[test]
    fn review_event_debug_comments_added() {
        let event = ReviewEvent::CommentsAdded {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            comments: vec![],
        };
        let debug = format!("{:?}", event);
        assert!(debug.contains("CommentsAdded"));
    }

    #[test]
    fn pr_review_state_new_accepts_string_types() {
        let state = PrReviewState::new(
            String::from("url"),
            String::from("repo"),
            42,
            String::from("issue"),
            String::from("linear"),
        );
        assert_eq!(state.pr_url, "url");
        assert_eq!(state.repo, "repo");
        assert_eq!(state.pr_number, 42);
        assert_eq!(state.issue_id, "issue");
        assert_eq!(state.source, "linear");
    }

    #[test]
    fn pr_review_state_new_accepts_str_types() {
        let state = PrReviewState::new("url", "repo", 0, "issue", "sentry");
        assert_eq!(state.pr_number, 0);
        assert_eq!(state.source, "sentry");
    }

    #[test]
    fn review_comment_debug_contains_fields() {
        let comment = make_review_comment("src/test.rs", "needs work", Some(42));
        let debug = format!("{:?}", comment);
        assert!(debug.contains("src/test.rs"));
        assert!(debug.contains("needs work"));
    }

    #[test]
    fn code_review_debug_contains_state() {
        let review = make_review("CHANGES_REQUESTED", Some("Please fix"));
        let debug = format!("{:?}", review);
        assert!(debug.contains("CHANGES_REQUESTED"));
        assert!(debug.contains("Please fix"));
    }

    #[test]
    fn compare_timestamps_empty_strings() {
        assert_eq!(compare_timestamps("", ""), Ordering::Equal);
    }

    #[test]
    fn compare_timestamps_one_empty() {
        // Empty string vs valid: falls back to lex comparison
        let valid = "2025-01-01T00:00:00Z";
        assert_eq!(compare_timestamps("", valid), Ordering::Less);
        assert_eq!(compare_timestamps(valid, ""), Ordering::Greater);
    }

    #[test]
    fn timestamp_at_or_after_both_empty() {
        assert!(timestamp_at_or_after("", ""));
    }

    // ================================================================
    // Additional coverage: ReviewWatcher constructors, check_for_pr
    // with active/disabled, PrMonitor with_regression_tracking,
    // empty trigger filter, standalone inline comments, cursor
    // advancement for comments, multiple interleaved reviews,
    // comment_is_after_cursor same-timestamp-same-id edge case.
    // ================================================================

    mod review_watcher_extended_tests {
        use crate::error::Result;
        use crate::scm::{
            CodeReview, PrInfo, PrReviewState, PrStatus, RemoteRepo, ReviewComment, ReviewEvent,
            ReviewUser, ReviewWatcher, ScmProvider,
        };
        use crate::storage::FixAttemptTracker;
        use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus};
        use async_trait::async_trait;
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        struct MockScmProvider {
            name: String,
            enabled: bool,
            trigger: String,
            reviews: Arc<Mutex<Vec<CodeReview>>>,
            comments: Arc<Mutex<Vec<ReviewComment>>>,
        }

        impl MockScmProvider {
            fn new(name: &str, enabled: bool, trigger: &str) -> Self {
                Self {
                    name: name.to_string(),
                    enabled,
                    trigger: trigger.to_string(),
                    reviews: Arc::new(Mutex::new(Vec::new())),
                    comments: Arc::new(Mutex::new(Vec::new())),
                }
            }

            fn set_reviews(&self, reviews: Vec<CodeReview>) {
                *self.reviews.lock().unwrap() = reviews;
            }

            fn set_comments(&self, comments: Vec<ReviewComment>) {
                *self.comments.lock().unwrap() = comments;
            }
        }

        #[async_trait]
        impl ScmProvider for MockScmProvider {
            fn name(&self) -> &str {
                &self.name
            }
            fn is_enabled(&self) -> bool {
                self.enabled
            }
            fn review_trigger(&self) -> &str {
                &self.trigger
            }
            async fn get_pr_status(&self, _project: &str, _number: i64) -> Result<PrStatus> {
                Ok(PrStatus::Open)
            }
            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: None,
                    base_branch: None,
                    title: None,
                    author: None,
                })
            }
            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok(String::new())
            }
            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(self.reviews.lock().unwrap().clone())
            }
            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                Ok(self.comments.lock().unwrap().clone())
            }
            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        struct MockTracker;

        impl FixAttemptTracker for MockTracker {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn has_attempted(&self, _source: &str, _issue_id: &str) -> Result<bool> {
                Ok(false)
            }
            fn get_attempted_issue_ids(&self, _source: &str) -> HashSet<String> {
                HashSet::new()
            }
            fn record_attempt(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn record_attempt_with_labels(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
                _labels: &[String],
            ) -> Result<()> {
                Ok(())
            }
            fn mark_success(&self, _source: &str, _issue_id: &str, _pr_url: &str) -> Result<()> {
                Ok(())
            }
            fn mark_failed(
                &self,
                _source: &str,
                _issue_id: &str,
                _error_message: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn mark_merged(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_closed(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_resolved(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_attempt(&self, _source: &str, _issue_id: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn get_attempts_by_status(&self, _status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn get_attempt_by_pr_url(&self, _pr_url: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn reset_attempt(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_stats(&self) -> Result<FixAttemptStats> {
                Ok(FixAttemptStats {
                    total: 0,
                    pending: 0,
                    success: 0,
                    failed: 0,
                    merged: 0,
                    closed: 0,
                    cannot_fix: 0,
                    by_source: Default::default(),
                })
            }
            fn increment_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_cannot_fix(&self, _source: &str, _issue_id: &str, _reason: &str) -> Result<()> {
                Ok(())
            }
            fn get_retryable_issues(&self, _max_retries: u32) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn prepare_for_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
        }

        fn make_review(id: i64, state: &str, user: &str, submitted_at: &str) -> CodeReview {
            CodeReview {
                id,
                state: state.to_string(),
                body: Some(format!("Review body {}", id)),
                user: ReviewUser {
                    id: id + 1000,
                    login: user.to_string(),
                    user_type: Some("User".to_string()),
                },
                submitted_at: Some(submitted_at.to_string()),
                html_url: Some(format!("https://github.com/org/repo/pull/1#review-{}", id)),
            }
        }

        fn make_comment(
            id: i64,
            body: &str,
            updated_at: &str,
            pull_request_review_id: Option<i64>,
        ) -> ReviewComment {
            ReviewComment {
                id,
                path: format!("src/file_{}.rs", id),
                position: Some(10),
                original_position: Some(10),
                body: body.to_string(),
                user: ReviewUser {
                    id: id + 2000,
                    login: "commenter".to_string(),
                    user_type: Some("User".to_string()),
                },
                created_at: updated_at.to_string(),
                updated_at: updated_at.to_string(),
                html_url: format!("https://github.com/org/repo/pull/1#comment-{}", id),
                pull_request_review_id,
                line: Some(10),
                start_line: None,
                side: Some("RIGHT".to_string()),
            }
        }

        fn make_state(pr_url: &str, repo: &str, pr_number: i64) -> PrReviewState {
            PrReviewState::new(pr_url, repo, pr_number, "ISSUE-1", "linear")
        }

        #[test]
        fn test_with_tracker_constructor() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTracker);
            let watcher = ReviewWatcher::with_tracker(provider, tracker);

            assert!(watcher.is_enabled());
            assert!(watcher.get_all_states().is_empty());
        }

        #[test]
        fn test_with_sqlite_tracker_constructor_none() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTracker);
            let watcher = ReviewWatcher::with_sqlite_tracker(provider, tracker, None);

            assert!(watcher.is_enabled());
            assert!(watcher.get_all_states().is_empty());
        }

        #[tokio::test]
        async fn test_check_for_pr_disabled() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", false, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher
                .check_for_pr("https://github.com/org/repo/pull/1")
                .await
                .unwrap();
            assert!(events.is_empty());
        }

        #[tokio::test]
        async fn test_check_for_pr_with_active_state() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher
                .check_for_pr("https://github.com/org/repo/pull/1")
                .await
                .unwrap();
            assert_eq!(events.len(), 1);
            assert!(matches!(&events[0], ReviewEvent::ReviewSubmitted { .. }));
        }

        #[tokio::test]
        async fn test_check_for_pr_inactive_state() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            // Watch then unwatch to remove the state entirely
            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));
            watcher.unwatch_pr("https://github.com/org/repo/pull/1");

            let events = watcher
                .check_for_pr("https://github.com/org/repo/pull/1")
                .await
                .unwrap();
            assert!(events.is_empty());
        }

        #[tokio::test]
        async fn test_empty_trigger_accepts_all_standalone_comments() {
            let mock = MockScmProvider::new("github", true, ""); // empty trigger
            mock.set_comments(vec![make_comment(
                1,
                "any old comment with no trigger",
                "2025-01-01T00:00:00Z",
                None, // standalone, not part of a review
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert_eq!(comments_events.len(), 1);
            if let ReviewEvent::CommentsAdded { comments, .. } = &comments_events[0] {
                assert_eq!(comments.len(), 1);
                assert_eq!(comments[0].id, 1);
            }
        }

        #[tokio::test]
        async fn test_standalone_inline_comment_from_older_review() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            // No reviews returned this cycle, but a comment references an
            // older review_id (999) that we did not process this cycle.
            mock.set_comments(vec![make_comment(
                50,
                "leftover inline feedback",
                "2025-01-01T00:00:00Z",
                Some(999), // review ID not in this cycle
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            // Should still produce a CommentsAdded event because inline
            // comments (pull_request_review_id.is_some()) are always
            // actionable.
            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert_eq!(comments_events.len(), 1);
            if let ReviewEvent::CommentsAdded { comments, .. } = &comments_events[0] {
                assert_eq!(comments.len(), 1);
                assert_eq!(comments[0].id, 50);
            }
        }

        #[tokio::test]
        async fn test_comment_cursor_advancement() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_comments(vec![
                make_comment(10, "@claudear fix A", "2025-01-01T00:00:00Z", None),
                make_comment(20, "@claudear fix B", "2025-01-02T00:00:00Z", None),
            ]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let _ = watcher.check_for_reviews().await.unwrap();

            // Cursor should have advanced to the latest comment
            let state = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(state.last_comment_id, Some(20));
            assert_eq!(
                state.last_comment_time,
                Some("2025-01-02T00:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_multiple_reviews_interleaved_ids() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            // IDs are out of order vs timestamps
            mock.set_reviews(vec![
                make_review(5, "COMMENTED", "alice", "2025-01-01T00:00:00Z"),
                make_review(3, "CHANGES_REQUESTED", "bob", "2025-01-02T00:00:00Z"),
                make_review(7, "APPROVED", "carol", "2025-01-03T00:00:00Z"),
            ]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            let review_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .collect();
            // All 3 should be processed (APPROVED does not require action
            // but still emits an event)
            assert_eq!(review_events.len(), 3);

            // After processing, cursor should reflect the max
            let state = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(state.last_review_id, Some(7));
            assert_eq!(
                state.last_review_time,
                Some("2025-01-03T00:00:00Z".to_string())
            );
        }

        #[test]
        fn test_comment_after_cursor_same_timestamp_same_id() {
            let comment = make_comment(5, "body", "2025-01-01T00:00:00Z", None);
            // Same timestamp, same id => not strictly after
            assert!(!ReviewWatcher::comment_is_after_cursor(
                &comment,
                Some("2025-01-01T00:00:00Z"),
                Some(5),
            ));
        }

        #[test]
        fn test_watch_pr_incoming_cursors_override_existing() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            // First watch with some cursors
            let mut state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            state.last_review_id = Some(10);
            state.last_review_time = Some("2025-01-01T00:00:00Z".to_string());
            watcher.watch_pr(state);

            // Re-watch with newer cursors already set
            let mut new_state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            new_state.last_review_id = Some(20);
            new_state.last_review_time = Some("2025-02-01T00:00:00Z".to_string());
            watcher.watch_pr(new_state);

            let retrieved = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            // Incoming cursor was set, so it wins
            assert_eq!(retrieved.last_review_id, Some(20));
            assert_eq!(
                retrieved.last_review_time,
                Some("2025-02-01T00:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_bot_comments_are_filtered() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let mut bot_comment =
                make_comment(1, "@claudear please fix", "2025-01-01T00:00:00Z", None);
            bot_comment.user.user_type = Some("Bot".to_string());
            mock.set_comments(vec![bot_comment]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            // Bot comments should be filtered out entirely
            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert!(comments_events.is_empty());
        }

        #[tokio::test]
        async fn test_trigger_case_insensitive() {
            let mock = MockScmProvider::new("github", true, "@Claudear");
            mock.set_comments(vec![make_comment(
                1,
                "hey @claudear please look",
                "2025-01-01T00:00:00Z",
                None,
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            let comments_events: Vec<_> = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::CommentsAdded { .. }))
                .collect();
            assert_eq!(comments_events.len(), 1);
        }

        #[tokio::test]
        async fn test_check_for_reviews_multiple_prs() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "COMMENTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));
            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/2",
                "org/repo",
                2,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            // Both PRs get the same mock data, so both should have events
            let review_count = events
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .count();
            assert_eq!(review_count, 2);
        }

        #[test]
        fn test_get_state_nonexistent() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);
            assert!(watcher.get_state("https://nonexistent").is_none());
        }

        #[test]
        fn test_load_states_overwrites() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);

            // Add one state manually
            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            // Load different states
            let loaded = vec![make_state(
                "https://github.com/org/repo/pull/99",
                "org/repo",
                99,
            )];
            watcher.load_states(loaded);

            let all = watcher.get_all_states();
            assert_eq!(all.len(), 2);
        }

        #[test]
        fn test_load_states_empty() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);
            watcher.load_states(vec![]);
            assert!(watcher.get_all_states().is_empty());
        }

        #[test]
        fn test_unwatch_nonexistent_pr_no_panic() {
            let provider: Arc<dyn ScmProvider> =
                Arc::new(MockScmProvider::new("github", true, "@claudear"));
            let watcher = ReviewWatcher::new(provider);
            // Should not panic
            watcher.unwatch_pr("https://nonexistent");
        }

        #[tokio::test]
        async fn test_with_tracker_records_review() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTracker);
            let watcher = ReviewWatcher::with_tracker(provider, tracker);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            // Should not panic even though MockTracker returns Ok(0)
            let events = watcher.check_for_reviews().await.unwrap();
            assert_eq!(events.len(), 1);
        }

        #[tokio::test]
        async fn test_second_poll_cycle_no_duplicates() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "COMMENTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let watcher = ReviewWatcher::new(Arc::clone(&provider));

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            // First poll
            let events1 = watcher.check_for_reviews().await.unwrap();
            assert_eq!(events1.len(), 1);

            // Second poll with same data - review already processed
            let events2 = watcher.check_for_reviews().await.unwrap();
            let review_count = events2
                .iter()
                .filter(|e| matches!(e, ReviewEvent::ReviewSubmitted { .. }))
                .count();
            assert_eq!(review_count, 0);
        }
    }

    mod pr_monitor_extended_tests {
        use super::*;
        use crate::error::Result;
        use crate::scm::{PrInfo, PrMonitor, PrStatus, RemoteRepo};
        use crate::storage::FixAttemptTracker;
        use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus};
        use async_trait::async_trait;
        use chrono::Utc;
        use std::collections::{HashMap, HashSet};
        use std::sync::{Arc, Mutex};

        struct MockPrScmProvider {
            enabled: bool,
            statuses: Arc<Mutex<HashMap<(String, i64), Result<PrStatus>>>>,
        }

        impl MockPrScmProvider {
            fn new(enabled: bool) -> Self {
                Self {
                    enabled,
                    statuses: Arc::new(Mutex::new(HashMap::new())),
                }
            }

            fn with_status(self, repo: &str, pr_number: i64, status: PrStatus) -> Self {
                self.statuses
                    .lock()
                    .unwrap()
                    .insert((repo.to_string(), pr_number), Ok(status));
                self
            }
        }

        #[async_trait]
        impl ScmProvider for MockPrScmProvider {
            fn name(&self) -> &str {
                "mock"
            }
            fn is_enabled(&self) -> bool {
                self.enabled
            }
            fn review_trigger(&self) -> &str {
                "/mock"
            }
            async fn get_pr_status(&self, project: &str, number: i64) -> Result<PrStatus> {
                let map = self.statuses.lock().unwrap();
                match map.get(&(project.to_string(), number)) {
                    Some(Ok(status)) => Ok(*status),
                    Some(Err(_)) => Err(crate::error::Error::Config("mock error".to_string())),
                    None => Ok(PrStatus::Open),
                }
            }
            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: None,
                    base_branch: None,
                    title: None,
                    author: None,
                })
            }
            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok(String::new())
            }
            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(vec![])
            }
            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                Ok(vec![])
            }
            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        struct MockFixAttemptTracker {
            pending_prs: Arc<Mutex<Vec<FixAttempt>>>,
            mark_merged_calls: Arc<Mutex<Vec<(String, String)>>>,
            mark_closed_calls: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl MockFixAttemptTracker {
            fn new(pending_prs: Vec<FixAttempt>) -> Self {
                Self {
                    pending_prs: Arc::new(Mutex::new(pending_prs)),
                    mark_merged_calls: Arc::new(Mutex::new(Vec::new())),
                    mark_closed_calls: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl FixAttemptTracker for MockFixAttemptTracker {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn has_attempted(&self, _source: &str, _issue_id: &str) -> Result<bool> {
                Ok(false)
            }
            fn get_attempted_issue_ids(&self, _source: &str) -> HashSet<String> {
                HashSet::new()
            }
            fn record_attempt(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn record_attempt_with_labels(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
                _labels: &[String],
            ) -> Result<()> {
                Ok(())
            }
            fn mark_success(&self, _source: &str, _issue_id: &str, _pr_url: &str) -> Result<()> {
                Ok(())
            }
            fn mark_failed(
                &self,
                _source: &str,
                _issue_id: &str,
                _error_message: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn mark_merged(&self, source: &str, issue_id: &str) -> Result<()> {
                self.mark_merged_calls
                    .lock()
                    .unwrap()
                    .push((source.to_string(), issue_id.to_string()));
                Ok(())
            }
            fn mark_closed(&self, source: &str, issue_id: &str) -> Result<()> {
                self.mark_closed_calls
                    .lock()
                    .unwrap()
                    .push((source.to_string(), issue_id.to_string()));
                Ok(())
            }
            fn mark_resolved(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_attempt(&self, _source: &str, _issue_id: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn get_attempts_by_status(&self, _status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
                Ok(self.pending_prs.lock().unwrap().clone())
            }
            fn get_attempt_by_pr_url(&self, _pr_url: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn reset_attempt(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_stats(&self) -> Result<FixAttemptStats> {
                Ok(FixAttemptStats {
                    total: 0,
                    pending: 0,
                    success: 0,
                    failed: 0,
                    merged: 0,
                    closed: 0,
                    cannot_fix: 0,
                    by_source: Default::default(),
                })
            }
            fn increment_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_cannot_fix(&self, _source: &str, _issue_id: &str, _reason: &str) -> Result<()> {
                Ok(())
            }
            fn get_retryable_issues(&self, _max_retries: u32) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn prepare_for_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
        }

        fn make_fix_attempt(
            source: &str,
            issue_id: &str,
            short_id: &str,
            repo: Option<&str>,
            pr_number: Option<i64>,
            pr_url: Option<&str>,
            labels: Vec<String>,
        ) -> FixAttempt {
            FixAttempt {
                id: 1,
                issue_id: issue_id.to_string(),
                short_id: short_id.to_string(),
                source: source.to_string(),
                attempted_at: Utc::now(),
                pr_url: pr_url.map(|u| u.to_string()),
                scm_repo: repo.map(|r| r.to_string()),
                scm_pr_number: pr_number,
                status: FixAttemptStatus::Success,
                error_message: None,
                merged_at: None,
                resolved_at: None,
                retry_count: 0,
                last_retry_at: None,
                issue_labels: labels,
                parent_attempt_id: None,
                cascade_repo: None,
            }
        }

        #[tokio::test]
        async fn test_with_regression_tracking_constructor() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Merged,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));

            let sqlite_tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());

            let monitor = PrMonitor::with_regression_tracking(
                provider,
                tracker.clone(),
                true,
                sqlite_tracker,
            );

            // Even with regression tracking enabled, creating the watch
            // will fail because the fix_attempts foreign key constraint
            // is not satisfied in this isolated test. The code handles
            // that gracefully (logs a warning and returns None).
            let attempt = make_fix_attempt(
                "sentry",
                "issue-bug",
                "SENTRY-BUG",
                Some("org/repo"),
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec!["bug".to_string()],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.new_status, PrStatus::Merged);
            // The regression watch creation fails due to FK constraint,
            // so it falls back to normal auto-resolve behavior.
            assert!(result.regression_watch_id.is_none());
            assert!(result.should_resolve);

            // Verify mark_merged was still called
            let merged_calls = tracker.mark_merged_calls.lock().unwrap();
            assert_eq!(merged_calls.len(), 1);
            assert_eq!(merged_calls[0].1, "issue-bug");
        }

        #[tokio::test]
        async fn test_is_bug_type_sentry() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt =
                make_fix_attempt("sentry", "issue-1", "SENTRY-1", None, None, None, vec![]);
            assert!(monitor.is_bug_type(&attempt));
        }

        #[tokio::test]
        async fn test_is_bug_type_linear_with_bug_label() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                None,
                None,
                None,
                vec!["bug".to_string()],
            );
            assert!(monitor.is_bug_type(&attempt));
        }

        #[tokio::test]
        async fn test_is_bug_type_linear_no_bug_label() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                None,
                None,
                None,
                vec!["feature".to_string()],
            );
            assert!(!monitor.is_bug_type(&attempt));
        }

        #[tokio::test]
        async fn test_merged_non_bug_no_regression_watch() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Merged,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));

            let sqlite_tracker = Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());

            let monitor = PrMonitor::with_regression_tracking(
                provider,
                tracker.clone(),
                true,
                sqlite_tracker,
            );

            // Linear with no bug labels - not a bug type, so regression
            // tracking does not apply
            let attempt = make_fix_attempt(
                "linear",
                "issue-feature",
                "LIN-FEAT",
                Some("org/repo"),
                Some(42),
                Some("https://github.com/org/repo/pull/42"),
                vec!["feature".to_string()],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.new_status, PrStatus::Merged);
            assert!(result.regression_watch_id.is_none());
            assert!(result.should_resolve); // auto_resolve is true and no regression watch
        }

        #[tokio::test]
        async fn test_merged_pr_url_defaults_empty() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Merged,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                Some("org/repo"),
                Some(42),
                None, // no pr_url
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.pr_url, "");
        }

        #[tokio::test]
        async fn test_closed_pr_url_defaults_empty() {
            let provider = Arc::new(MockPrScmProvider::new(true).with_status(
                "org/repo",
                42,
                PrStatus::Closed,
            ));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let attempt = make_fix_attempt(
                "linear",
                "issue-1",
                "LIN-1",
                Some("org/repo"),
                Some(42),
                None,
                vec![],
            );

            let result = monitor.check_pr(&attempt).await.unwrap().unwrap();
            assert_eq!(result.pr_url, "");
            assert_eq!(result.new_status, PrStatus::Closed);
        }

        #[tokio::test]
        async fn test_check_pending_prs_empty() {
            let provider = Arc::new(MockPrScmProvider::new(true));
            let tracker = Arc::new(MockFixAttemptTracker::new(vec![]));
            let monitor = PrMonitor::new(provider, tracker, true);

            let updates = monitor.check_pending_prs().await.unwrap();
            assert!(updates.is_empty());
        }
    }

    #[test]
    fn review_event_requires_action_unknown_state() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("SOME_UNKNOWN_STATE", None),
            inline_comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn review_event_requires_action_empty_state() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("", None),
            inline_comments: vec![],
        };
        assert!(!event.requires_action());
    }

    #[test]
    fn pr_status_exhaustive_match() {
        for status in [PrStatus::Open, PrStatus::Merged, PrStatus::Closed] {
            match status {
                PrStatus::Open => assert_eq!(status, PrStatus::Open),
                PrStatus::Merged => assert_eq!(status, PrStatus::Merged),
                PrStatus::Closed => assert_eq!(status, PrStatus::Closed),
            }
        }
    }

    #[test]
    fn pr_info_clone() {
        let info = PrInfo {
            head_branch: Some("feat".to_string()),
            base_branch: Some("main".to_string()),
            title: Some("title".to_string()),
            author: Some("author".to_string()),
        };
        let cloned = info.clone();
        assert_eq!(cloned.head_branch.as_deref(), Some("feat"));
        assert_eq!(cloned.base_branch.as_deref(), Some("main"));
        assert_eq!(cloned.title.as_deref(), Some("title"));
        assert_eq!(cloned.author.as_deref(), Some("author"));
    }

    #[test]
    fn pr_status_update_clone() {
        let update = PrStatusUpdate {
            source: "sentry".to_string(),
            issue_id: "id".to_string(),
            short_id: "sid".to_string(),
            pr_url: "url".to_string(),
            new_status: PrStatus::Merged,
            should_resolve: true,
            regression_watch_id: Some(1),
        };
        let cloned = update.clone();
        assert_eq!(cloned.source, "sentry");
        assert_eq!(cloned.new_status, PrStatus::Merged);
        assert_eq!(cloned.regression_watch_id, Some(1));
    }

    #[test]
    fn review_event_feedback_summary_multiple_inline_comments_with_lines() {
        let event = ReviewEvent::ReviewSubmitted {
            pr_url: "url".to_string(),
            repo: "repo".to_string(),
            pr_number: 1,
            review: make_review("CHANGES_REQUESTED", Some("Overall, needs work.")),
            inline_comments: vec![
                make_review_comment("src/a.rs", "fix error handling", Some(10)),
                make_review_comment("src/b.rs", "add docs", Some(20)),
                make_review_comment("src/c.rs", "remove debug print", None),
            ],
        };
        let summary = event.get_feedback_summary();
        assert!(summary.contains("Overall, needs work."));
        assert!(summary.contains("Inline comments (3):"));
        assert!(summary.contains("`src/a.rs`"));
        assert!(summary.contains("(line 10)"));
        assert!(summary.contains("`src/b.rs`"));
        assert!(summary.contains("(line 20)"));
        assert!(summary.contains("`src/c.rs`"));
        assert!(summary.contains("remove debug print"));
    }

    #[test]
    fn remote_repo_private_archived_flags() {
        let repo = RemoteRepo {
            id: 1,
            full_name: "org/private-archived".to_string(),
            name: "private-archived".to_string(),
            default_branch: "main".to_string(),
            clone_url: "https://github.com/org/private-archived.git".to_string(),
            ssh_url: "git@github.com:org/private-archived.git".to_string(),
            html_url: "https://github.com/org/private-archived".to_string(),
            private: true,
            archived: true,
        };
        assert!(repo.private);
        assert!(repo.archived);

        let json = serde_json::to_string(&repo).unwrap();
        let deserialized: RemoteRepo = serde_json::from_str(&json).unwrap();
        assert!(deserialized.private);
        assert!(deserialized.archived);
    }

    #[test]
    fn compare_timestamps_different_days() {
        let jan = "2025-01-15T00:00:00Z";
        let feb = "2025-02-15T00:00:00Z";
        assert_eq!(compare_timestamps(jan, feb), std::cmp::Ordering::Less);
        assert_eq!(compare_timestamps(feb, jan), std::cmp::Ordering::Greater);
    }

    #[test]
    fn compare_timestamps_different_years() {
        let y2024 = "2024-06-15T12:00:00Z";
        let y2025 = "2025-06-15T12:00:00Z";
        assert_eq!(compare_timestamps(y2024, y2025), std::cmp::Ordering::Less);
    }

    #[test]
    fn code_review_clone() {
        let review = CodeReview {
            id: 42,
            state: "APPROVED".to_string(),
            body: Some("lgtm".to_string()),
            user: ReviewUser {
                id: 7,
                login: "dev".to_string(),
                user_type: Some("User".to_string()),
            },
            submitted_at: Some("2025-01-01T00:00:00Z".to_string()),
            html_url: Some("https://url".to_string()),
        };
        let cloned = review.clone();
        assert_eq!(cloned.id, 42);
        assert_eq!(cloned.state, "APPROVED");
        assert_eq!(cloned.body.as_deref(), Some("lgtm"));
        assert_eq!(cloned.user.id, 7);
        assert_eq!(cloned.user.login, "dev");
        assert_eq!(cloned.submitted_at.as_deref(), Some("2025-01-01T00:00:00Z"));
    }

    #[test]
    fn review_comment_clone() {
        let comment = ReviewComment {
            id: 99,
            path: "src/test.rs".to_string(),
            position: Some(5),
            original_position: Some(5),
            body: "fix".to_string(),
            user: ReviewUser {
                id: 1,
                login: "u".to_string(),
                user_type: None,
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-02T00:00:00Z".to_string(),
            html_url: "https://url".to_string(),
            pull_request_review_id: Some(10),
            line: Some(5),
            start_line: Some(3),
            side: Some("LEFT".to_string()),
        };
        let cloned = comment.clone();
        assert_eq!(cloned.id, 99);
        assert_eq!(cloned.path, "src/test.rs");
        assert_eq!(cloned.position, Some(5));
        assert_eq!(cloned.original_position, Some(5));
        assert_eq!(cloned.pull_request_review_id, Some(10));
        assert_eq!(cloned.line, Some(5));
        assert_eq!(cloned.start_line, Some(3));
        assert_eq!(cloned.side.as_deref(), Some("LEFT"));
    }

    // ================================================================
    // Additional coverage: record_review_to_db, with_sqlite_tracker
    // persistence, check_pr_reviews with sqlite, PrMonitor
    // activity log recording, and ReviewWatcher persistence paths.
    // ================================================================

    mod sqlite_persistence_tests {
        use crate::error::Result;
        use crate::scm::{
            CodeReview, PrInfo, PrReviewState, PrStatus, RemoteRepo, ReviewComment, ReviewUser,
            ReviewWatcher, ScmProvider,
        };
        use crate::storage::{FixAttemptTracker, SqliteTracker};
        use crate::types::{
            ActivityLogEntry, FixAttempt, FixAttemptStats, FixAttemptStatus, PrReviewRecord,
        };
        use async_trait::async_trait;
        use std::collections::HashSet;
        use std::sync::{Arc, Mutex};

        struct MockScmProvider {
            name: String,
            enabled: bool,
            trigger: String,
            reviews: Arc<Mutex<Vec<CodeReview>>>,
            comments: Arc<Mutex<Vec<ReviewComment>>>,
        }

        impl MockScmProvider {
            fn new(name: &str, enabled: bool, trigger: &str) -> Self {
                Self {
                    name: name.to_string(),
                    enabled,
                    trigger: trigger.to_string(),
                    reviews: Arc::new(Mutex::new(Vec::new())),
                    comments: Arc::new(Mutex::new(Vec::new())),
                }
            }

            fn set_reviews(&self, reviews: Vec<CodeReview>) {
                *self.reviews.lock().unwrap() = reviews;
            }

            fn set_comments(&self, comments: Vec<ReviewComment>) {
                *self.comments.lock().unwrap() = comments;
            }
        }

        #[async_trait]
        impl ScmProvider for MockScmProvider {
            fn name(&self) -> &str {
                &self.name
            }
            fn is_enabled(&self) -> bool {
                self.enabled
            }
            fn review_trigger(&self) -> &str {
                &self.trigger
            }
            async fn get_pr_status(&self, _project: &str, _number: i64) -> Result<PrStatus> {
                Ok(PrStatus::Open)
            }
            async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                Ok(PrInfo {
                    head_branch: None,
                    base_branch: None,
                    title: None,
                    author: None,
                })
            }
            async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                Ok(String::new())
            }
            async fn get_reviews(&self, _project: &str, _number: i64) -> Result<Vec<CodeReview>> {
                Ok(self.reviews.lock().unwrap().clone())
            }
            async fn get_review_comments(
                &self,
                _project: &str,
                _number: i64,
            ) -> Result<Vec<ReviewComment>> {
                Ok(self.comments.lock().unwrap().clone())
            }
            async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                Ok(vec![])
            }
        }

        struct MockTrackerWithRecording {
            review_calls: Arc<Mutex<Vec<String>>>,
            activity_calls: Arc<Mutex<Vec<String>>>,
        }

        impl MockTrackerWithRecording {
            fn new() -> Self {
                Self {
                    review_calls: Arc::new(Mutex::new(Vec::new())),
                    activity_calls: Arc::new(Mutex::new(Vec::new())),
                }
            }
        }

        impl FixAttemptTracker for MockTrackerWithRecording {
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn has_attempted(&self, _source: &str, _issue_id: &str) -> Result<bool> {
                Ok(false)
            }
            fn get_attempted_issue_ids(&self, _source: &str) -> HashSet<String> {
                HashSet::new()
            }
            fn record_attempt(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn record_attempt_with_labels(
                &self,
                _source: &str,
                _issue_id: &str,
                _short_id: &str,
                _labels: &[String],
            ) -> Result<()> {
                Ok(())
            }
            fn mark_success(&self, _source: &str, _issue_id: &str, _pr_url: &str) -> Result<()> {
                Ok(())
            }
            fn mark_failed(
                &self,
                _source: &str,
                _issue_id: &str,
                _error_message: &str,
            ) -> Result<()> {
                Ok(())
            }
            fn mark_merged(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_closed(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_resolved(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_attempt(&self, _source: &str, _issue_id: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn get_attempts_by_status(&self, _status: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn get_attempt_by_pr_url(&self, _pr_url: &str) -> Result<Option<FixAttempt>> {
                Ok(None)
            }
            fn reset_attempt(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn get_stats(&self) -> Result<FixAttemptStats> {
                Ok(FixAttemptStats {
                    total: 0,
                    pending: 0,
                    success: 0,
                    failed: 0,
                    merged: 0,
                    closed: 0,
                    cannot_fix: 0,
                    by_source: Default::default(),
                })
            }
            fn increment_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn mark_cannot_fix(&self, _source: &str, _issue_id: &str, _reason: &str) -> Result<()> {
                Ok(())
            }
            fn get_retryable_issues(&self, _max_retries: u32) -> Result<Vec<FixAttempt>> {
                Ok(vec![])
            }
            fn prepare_for_retry(&self, _source: &str, _issue_id: &str) -> Result<()> {
                Ok(())
            }
            fn record_pr_review(&self, review: &PrReviewRecord) -> Result<i64> {
                self.review_calls
                    .lock()
                    .unwrap()
                    .push(review.pr_url.clone());
                Ok(1)
            }
            fn record_activity(&self, entry: &ActivityLogEntry) -> Result<i64> {
                self.activity_calls
                    .lock()
                    .unwrap()
                    .push(entry.activity_type.clone());
                Ok(1)
            }
        }

        fn make_review(id: i64, state: &str, user: &str, submitted_at: &str) -> CodeReview {
            CodeReview {
                id,
                state: state.to_string(),
                body: Some(format!("Review body {}", id)),
                user: ReviewUser {
                    id: id + 1000,
                    login: user.to_string(),
                    user_type: Some("User".to_string()),
                },
                submitted_at: Some(submitted_at.to_string()),
                html_url: Some(format!("https://github.com/org/repo/pull/1#review-{}", id)),
            }
        }

        fn make_comment(
            id: i64,
            body: &str,
            updated_at: &str,
            pull_request_review_id: Option<i64>,
        ) -> ReviewComment {
            ReviewComment {
                id,
                path: format!("src/file_{}.rs", id),
                position: Some(10),
                original_position: Some(10),
                body: body.to_string(),
                user: ReviewUser {
                    id: id + 2000,
                    login: "commenter".to_string(),
                    user_type: Some("User".to_string()),
                },
                created_at: updated_at.to_string(),
                updated_at: updated_at.to_string(),
                html_url: format!("https://github.com/org/repo/pull/1#comment-{}", id),
                pull_request_review_id,
                line: Some(10),
                start_line: None,
                side: Some("RIGHT".to_string()),
            }
        }

        fn make_state(pr_url: &str, repo: &str, pr_number: i64) -> PrReviewState {
            PrReviewState::new(pr_url, repo, pr_number, "ISSUE-1", "linear")
        }

        #[tokio::test]
        async fn test_record_review_to_db_calls_tracker() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker = Arc::new(MockTrackerWithRecording::new());
            let watcher = ReviewWatcher::with_tracker(provider, tracker.clone());

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            assert_eq!(events.len(), 1);

            // Verify record_pr_review was called
            let review_calls = tracker.review_calls.lock().unwrap();
            assert_eq!(review_calls.len(), 1);
            assert_eq!(review_calls[0], "https://github.com/org/repo/pull/1");

            // Verify record_activity was called
            let activity_calls = tracker.activity_calls.lock().unwrap();
            assert_eq!(activity_calls.len(), 1);
            assert_eq!(activity_calls[0], "pr_review_received");
        }

        #[tokio::test]
        async fn test_record_review_to_db_no_submitted_at() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let mut review = make_review(1, "COMMENTED", "reviewer", "2025-01-01T00:00:00Z");
            review.submitted_at = None;
            mock.set_reviews(vec![review]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker = Arc::new(MockTrackerWithRecording::new());
            let watcher = ReviewWatcher::with_tracker(provider, tracker.clone());

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            // Review without submitted_at should still be processed
            assert_eq!(events.len(), 1);

            let review_calls = tracker.review_calls.lock().unwrap();
            assert_eq!(review_calls.len(), 1);
        }

        #[tokio::test]
        async fn test_with_sqlite_tracker_persists_watch() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTrackerWithRecording::new());
            let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());

            let watcher =
                ReviewWatcher::with_sqlite_tracker(provider, tracker, Some(sqlite.clone()));

            let state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            watcher.watch_pr(state);

            // State should be in the watcher
            let retrieved = watcher.get_state("https://github.com/org/repo/pull/1");
            assert!(retrieved.is_some());

            // Verify it was persisted to sqlite
            let db_states = sqlite.get_active_pr_review_states().unwrap_or_default();
            assert_eq!(db_states.len(), 1);
            assert_eq!(db_states[0].pr_url, "https://github.com/org/repo/pull/1");
        }

        #[tokio::test]
        async fn test_with_sqlite_tracker_persists_unwatch() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTrackerWithRecording::new());
            let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());

            let watcher =
                ReviewWatcher::with_sqlite_tracker(provider, tracker, Some(sqlite.clone()));

            let state = make_state("https://github.com/org/repo/pull/1", "org/repo", 1);
            watcher.watch_pr(state);
            watcher.unwatch_pr("https://github.com/org/repo/pull/1");

            // In-memory state should be removed
            assert!(watcher
                .get_state("https://github.com/org/repo/pull/1")
                .is_none());

            // DB state should be deactivated
            let db_states = sqlite.get_active_pr_review_states().unwrap_or_default();
            assert_eq!(db_states.len(), 0);
        }

        #[tokio::test]
        async fn test_with_sqlite_tracker_persists_cursor_advance() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![make_review(
                1,
                "CHANGES_REQUESTED",
                "reviewer",
                "2025-01-01T00:00:00Z",
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTrackerWithRecording::new());
            let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());

            let watcher =
                ReviewWatcher::with_sqlite_tracker(provider, tracker, Some(sqlite.clone()));

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let _ = watcher.check_for_reviews().await.unwrap();

            // Verify cursor was advanced and persisted
            let state = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(state.last_review_id, Some(1));

            // Check the DB state has the cursor too
            let db_states = sqlite.get_active_pr_review_states().unwrap_or_default();
            assert_eq!(db_states.len(), 1);
            assert_eq!(db_states[0].last_review_id, Some(1));
        }

        #[tokio::test]
        async fn test_with_sqlite_tracker_persists_comment_cursor() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_comments(vec![make_comment(
                10,
                "@claudear fix this",
                "2025-01-01T00:00:00Z",
                None,
            )]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker: Arc<dyn FixAttemptTracker> = Arc::new(MockTrackerWithRecording::new());
            let sqlite = Arc::new(SqliteTracker::in_memory().unwrap());

            let watcher =
                ReviewWatcher::with_sqlite_tracker(provider, tracker, Some(sqlite.clone()));

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let _ = watcher.check_for_reviews().await.unwrap();

            // Verify comment cursor was advanced
            let state = watcher
                .get_state("https://github.com/org/repo/pull/1")
                .unwrap();
            assert_eq!(state.last_comment_id, Some(10));
            assert_eq!(
                state.last_comment_time,
                Some("2025-01-01T00:00:00Z".to_string())
            );
        }

        #[tokio::test]
        async fn test_multiple_reviews_record_each_to_db() {
            let mock = MockScmProvider::new("github", true, "@claudear");
            mock.set_reviews(vec![
                make_review(1, "COMMENTED", "alice", "2025-01-01T00:00:00Z"),
                make_review(2, "CHANGES_REQUESTED", "bob", "2025-01-02T00:00:00Z"),
            ]);
            let provider: Arc<dyn ScmProvider> = Arc::new(mock);
            let tracker = Arc::new(MockTrackerWithRecording::new());
            let watcher = ReviewWatcher::with_tracker(provider, tracker.clone());

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            let events = watcher.check_for_reviews().await.unwrap();
            assert_eq!(events.len(), 2);

            // Each review should be recorded individually
            let review_calls = tracker.review_calls.lock().unwrap();
            assert_eq!(review_calls.len(), 2);

            let activity_calls = tracker.activity_calls.lock().unwrap();
            assert_eq!(activity_calls.len(), 2);
        }

        #[tokio::test]
        async fn test_check_for_reviews_handles_provider_error() {
            // Use a provider that returns an error for get_reviews
            struct ErrorProvider;
            #[async_trait]
            impl ScmProvider for ErrorProvider {
                fn name(&self) -> &str {
                    "error"
                }
                fn is_enabled(&self) -> bool {
                    true
                }
                fn review_trigger(&self) -> &str {
                    "@claudear"
                }
                async fn get_pr_status(&self, _project: &str, _number: i64) -> Result<PrStatus> {
                    Ok(PrStatus::Open)
                }
                async fn get_pr_info(&self, _project: &str, _number: i64) -> Result<PrInfo> {
                    Ok(PrInfo {
                        head_branch: None,
                        base_branch: None,
                        title: None,
                        author: None,
                    })
                }
                async fn get_pr_diff(&self, _project: &str, _number: i64) -> Result<String> {
                    Ok(String::new())
                }
                async fn get_reviews(
                    &self,
                    _project: &str,
                    _number: i64,
                ) -> Result<Vec<CodeReview>> {
                    Err(crate::error::Error::Source {
                        source_name: "error".to_string(),
                        message: "simulated error".to_string(),
                    })
                }
                async fn get_review_comments(
                    &self,
                    _project: &str,
                    _number: i64,
                ) -> Result<Vec<ReviewComment>> {
                    Ok(vec![])
                }
                async fn list_repos(&self, _org_or_group: &str) -> Result<Vec<RemoteRepo>> {
                    Ok(vec![])
                }
            }

            let provider: Arc<dyn ScmProvider> = Arc::new(ErrorProvider);
            let watcher = ReviewWatcher::new(provider);

            watcher.watch_pr(make_state(
                "https://github.com/org/repo/pull/1",
                "org/repo",
                1,
            ));

            // Should not propagate the error; just logs it and returns empty
            let events = watcher.check_for_reviews().await.unwrap();
            assert!(events.is_empty());
        }
    }
}

// Backward-compatibility aliases

/// Alias for backward compatibility (was `PrReview` in github.rs).
pub type PrReview = CodeReview;
/// Alias for backward compatibility (was `PrReviewComment` in github.rs).
pub type PrReviewComment = ReviewComment;
/// Alias for backward compatibility (was `GitHubUser` in github.rs).
pub type GitHubUser = ReviewUser;
/// Alias for backward compatibility (was `OrgRepo` in github.rs).
pub type OrgRepo = RemoteRepo;

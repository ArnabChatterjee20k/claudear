//! Core types shared across the application.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Maximum allowed length for issue IDs to prevent DoS attacks.
pub const MAX_ISSUE_ID_LENGTH: usize = 100;

/// Validate an issue ID for safety and sanity.
///
/// Returns `Ok(())` if the issue ID is valid, or `Err` with a description of the problem.
///
/// # Validation Rules
/// - Must not be empty
/// - Must not exceed `MAX_ISSUE_ID_LENGTH` (100) characters
/// - Must not contain path traversal sequences (`..`)
/// - Must not contain forward slashes (`/`)
/// - Must not contain backslashes (`\`)
/// - Must not contain null bytes
///
/// # Examples
/// ```
/// use claudear::types::validate_issue_id;
///
/// assert!(validate_issue_id("PROJ-123").is_ok());
/// assert!(validate_issue_id("abc123").is_ok());
/// assert!(validate_issue_id("").is_err());
/// assert!(validate_issue_id("../etc/passwd").is_err());
/// assert!(validate_issue_id("a/b").is_err());
/// ```
pub fn validate_issue_id(issue_id: &str) -> Result<(), String> {
    if issue_id.is_empty() {
        return Err("Issue ID cannot be empty".to_string());
    }

    if issue_id.len() > MAX_ISSUE_ID_LENGTH {
        return Err(format!(
            "Issue ID exceeds maximum length of {} characters",
            MAX_ISSUE_ID_LENGTH
        ));
    }

    if issue_id.contains("..") {
        return Err("Issue ID cannot contain path traversal sequences (..)".to_string());
    }

    if issue_id.contains('/') {
        return Err("Issue ID cannot contain forward slashes (/)".to_string());
    }

    if issue_id.contains('\\') {
        return Err("Issue ID cannot contain backslashes (\\)".to_string());
    }

    if issue_id.contains('\0') {
        return Err("Issue ID cannot contain null bytes".to_string());
    }

    Ok(())
}

/// Priority levels for issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum IssuePriority {
    #[default]
    None,
    Low,
    Medium,
    High,
    Critical,
}

impl std::fmt::Display for IssuePriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

/// Status of an issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IssueStatus {
    #[default]
    Open,
    InProgress,
    Resolved,
    Ignored,
}

impl std::fmt::Display for IssueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => write!(f, "open"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Resolved => write!(f, "resolved"),
            Self::Ignored => write!(f, "ignored"),
        }
    }
}

/// Unified issue representation across all sources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Unique identifier from the source.
    pub id: String,
    /// Human-readable identifier (e.g., "PROJ-123", "SENTRY-ABC").
    pub short_id: String,
    /// Issue title.
    pub title: String,
    /// Issue description or error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// URL to view the issue in its source.
    pub url: String,
    /// Source service name.
    pub source: String,
    /// Priority level.
    pub priority: IssuePriority,
    /// Current status.
    pub status: IssueStatus,
    /// Additional metadata specific to the source.
    #[serde(default)]
    pub metadata: HashMap<String, serde_json::Value>,
    /// When the issue was first seen/created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// When the issue was last updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Issue {
    /// Create a new issue with required fields.
    pub fn new(
        id: impl Into<String>,
        short_id: impl Into<String>,
        title: impl Into<String>,
        url: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            short_id: short_id.into(),
            title: title.into(),
            description: None,
            url: url.into(),
            source: source.into(),
            priority: IssuePriority::default(),
            status: IssueStatus::default(),
            metadata: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Get a metadata value as a specific type.
    pub fn get_metadata<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.metadata
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Set a metadata value.
    pub fn set_metadata(&mut self, key: impl Into<String>, value: impl Serialize) {
        if let Ok(v) = serde_json::to_value(value) {
            self.metadata.insert(key.into(), v);
        }
    }
}

/// Priority for processing order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MatchPriority {
    Low,
    #[default]
    Normal,
    High,
    Urgent,
}

/// Result of matching an issue against criteria.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchResult {
    /// Whether the issue matches.
    pub matches: bool,
    /// Human-readable reason for the match result.
    pub reason: String,
    /// Priority classification for processing order.
    pub priority: MatchPriority,
}

impl MatchResult {
    /// Create a matching result.
    pub fn matched(reason: impl Into<String>, priority: MatchPriority) -> Self {
        Self {
            matches: true,
            reason: reason.into(),
            priority,
        }
    }

    /// Create a non-matching result.
    pub fn not_matched(reason: impl Into<String>) -> Self {
        Self {
            matches: false,
            reason: reason.into(),
            priority: MatchPriority::Normal,
        }
    }
}

/// Result of a Claude fix attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeResult {
    /// Whether the fix was successful.
    pub success: bool,
    /// Raw output from Claude.
    pub output: String,
    /// Extracted PR URL if available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Status of a fix attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FixAttemptStatus {
    Pending,
    Success,
    Failed,
    /// PR was merged and issue was resolved.
    Merged,
    /// PR was closed without merging.
    Closed,
    /// Max retries reached, issue cannot be automatically fixed.
    CannotFix,
}

impl std::fmt::Display for FixAttemptStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Success => write!(f, "success"),
            Self::Failed => write!(f, "failed"),
            Self::Merged => write!(f, "merged"),
            Self::Closed => write!(f, "closed"),
            Self::CannotFix => write!(f, "cannot_fix"),
        }
    }
}

impl std::str::FromStr for FixAttemptStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            "cannot_fix" => Ok(Self::CannotFix),
            _ => Err(format!("Unknown status: {}", s)),
        }
    }
}

/// Record of a fix attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixAttempt {
    pub id: i64,
    /// Issue ID from the source.
    pub issue_id: String,
    /// Human-readable issue ID.
    pub short_id: String,
    /// Source service name.
    pub source: String,
    /// When the attempt was made.
    pub attempted_at: DateTime<Utc>,
    /// PR URL if successful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// GitHub repository (owner/repo format).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_repo: Option<String>,
    /// GitHub PR number.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_pr_number: Option<i64>,
    /// Current status.
    pub status: FixAttemptStatus,
    /// Error message if failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// When the PR was merged (if merged).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<DateTime<Utc>>,
    /// When the issue was resolved on the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// Number of retry attempts made.
    #[serde(default)]
    pub retry_count: u32,
    /// When the last retry was attempted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_retry_at: Option<DateTime<Utc>>,
}

/// Statistics about fix attempts.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FixAttemptStats {
    pub total: usize,
    pub pending: usize,
    pub success: usize,
    pub failed: usize,
    /// PRs that were merged successfully.
    pub merged: usize,
    /// PRs that were closed without merging.
    pub closed: usize,
    /// Issues that reached max retries and cannot be fixed.
    pub cannot_fix: usize,
    pub by_source: HashMap<String, SourceStats>,
}

/// Per-source statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStats {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub merged: usize,
    pub closed: usize,
    pub cannot_fix: usize,
}

/// Activity log entry for tracking operational events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityLogEntry {
    /// Database ID.
    pub id: i64,
    /// When the activity occurred.
    pub timestamp: DateTime<Utc>,
    /// Type of activity (e.g., 'issue_received', 'processing_started', 'pr_created', 'error').
    pub activity_type: String,
    /// Source service (e.g., 'linear', 'sentry').
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Issue ID from the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// Human-readable issue ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_id: Option<String>,
    /// Human-readable message describing the activity.
    pub message: String,
    /// Additional context as JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

impl ActivityLogEntry {
    /// Create a new activity log entry.
    pub fn new(activity_type: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: 0,
            timestamp: Utc::now(),
            activity_type: activity_type.into(),
            source: None,
            issue_id: None,
            short_id: None,
            message: message.into(),
            metadata: None,
        }
    }

    /// Set the source for this activity.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set the issue ID for this activity.
    pub fn with_issue(mut self, issue_id: impl Into<String>, short_id: impl Into<String>) -> Self {
        self.issue_id = Some(issue_id.into());
        self.short_id = Some(short_id.into());
        self
    }

    /// Set metadata for this activity.
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// Claude execution record with detailed metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeExecution {
    /// Database ID.
    pub id: i64,
    /// Reference to fix_attempts table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<i64>,
    /// When execution started.
    pub started_at: DateTime<Utc>,
    /// When execution completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// Duration in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Process exit code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Whether the process timed out.
    pub timed_out: bool,
    /// Preview of stdout (first/last N chars).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_preview: Option<String>,
    /// Preview of stderr.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_preview: Option<String>,
    /// The prompt sent to Claude.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_used: Option<String>,
    /// Hash of the prompt for grouping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_hash: Option<String>,
    /// Claude model version used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    /// Working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    /// Git branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    /// Git commit hash before execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_before: Option<String>,
    /// Git commit hash after execution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_after: Option<String>,
    /// Number of files changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<i32>,
    /// Lines added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<i32>,
    /// Lines removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<i32>,
}

impl ClaudeExecution {
    /// Create a new execution record with the start time set to now.
    pub fn new() -> Self {
        Self {
            id: 0,
            attempt_id: None,
            started_at: Utc::now(),
            completed_at: None,
            duration_secs: None,
            exit_code: None,
            timed_out: false,
            stdout_preview: None,
            stderr_preview: None,
            prompt_used: None,
            prompt_hash: None,
            model_version: None,
            working_directory: None,
            git_branch: None,
            git_commit_before: None,
            git_commit_after: None,
            files_changed: None,
            lines_added: None,
            lines_removed: None,
        }
    }

    /// Set the attempt ID.
    pub fn with_attempt_id(mut self, attempt_id: i64) -> Self {
        self.attempt_id = Some(attempt_id);
        self
    }

    /// Mark the execution as complete.
    pub fn complete(&mut self, exit_code: Option<i32>, timed_out: bool) {
        let now = Utc::now();
        self.completed_at = Some(now);
        self.duration_secs = Some((now - self.started_at).num_milliseconds() as f64 / 1000.0);
        self.exit_code = exit_code;
        self.timed_out = timed_out;
    }
}

impl Default for ClaudeExecution {
    fn default() -> Self {
        Self::new()
    }
}

/// PR review feedback for learning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewRecord {
    /// Database ID.
    pub id: i64,
    /// Reference to fix_attempts table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<i64>,
    /// PR URL.
    pub pr_url: String,
    /// Reviewer username.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    /// Review state (approved, changes_requested, commented).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_state: Option<String>,
    /// When the review was submitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submitted_at: Option<DateTime<Utc>>,
    /// Review body/comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Computed sentiment (positive, negative, neutral).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sentiment: Option<String>,
    /// Extracted improvement suggestions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actionable_feedback: Option<String>,
}

impl PrReviewRecord {
    /// Create a new PR review record.
    pub fn new(pr_url: impl Into<String>) -> Self {
        Self {
            id: 0,
            attempt_id: None,
            pr_url: pr_url.into(),
            reviewer: None,
            review_state: None,
            submitted_at: None,
            body: None,
            sentiment: None,
            actionable_feedback: None,
        }
    }
}

/// PR lifecycle tracking record.
///
/// Tracks comprehensive information about a PR from creation to merge/close.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrRecord {
    /// Database ID.
    pub id: i64,
    /// Full PR URL.
    pub pr_url: String,
    /// GitHub repository (owner/repo).
    pub github_repo: String,
    /// PR number.
    pub pr_number: i64,

    /// Reference to fix_attempts table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<i64>,
    /// Original issue ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    /// Original issue source (linear, sentry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_source: Option<String>,

    /// PR title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// PR description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// PR author.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Head branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    /// Base branch name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,

    /// PR status: open, merged, closed.
    pub status: String,
    /// When the PR was created.
    pub created_at: DateTime<Utc>,
    /// When the PR was last updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// When the PR was merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<DateTime<Utc>>,
    /// When the PR was closed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,

    /// Number of approvals.
    pub approvals_count: i32,
    /// Number of changes_requested reviews.
    pub changes_requested_count: i32,
    /// Number of comments.
    pub comments_count: i32,
    /// When the last review was submitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_review_at: Option<DateTime<Utc>>,

    /// Minutes from PR creation to first review.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_review_mins: Option<i64>,
    /// Minutes from PR creation to merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_merge_mins: Option<i64>,
    /// Number of review cycles.
    pub review_cycles: i32,

    /// Number of files changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<i64>,
    /// Lines added.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<i64>,
    /// Lines removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<i64>,
}

impl PrRecord {
    /// Create a new PR record.
    pub fn new(pr_url: impl Into<String>, github_repo: impl Into<String>, pr_number: i64) -> Self {
        Self {
            id: 0,
            pr_url: pr_url.into(),
            github_repo: github_repo.into(),
            pr_number,
            attempt_id: None,
            issue_id: None,
            issue_source: None,
            title: None,
            description: None,
            author: None,
            head_branch: None,
            base_branch: None,
            status: "open".to_string(),
            created_at: Utc::now(),
            updated_at: None,
            merged_at: None,
            closed_at: None,
            approvals_count: 0,
            changes_requested_count: 0,
            comments_count: 0,
            last_review_at: None,
            time_to_first_review_mins: None,
            time_to_merge_mins: None,
            review_cycles: 0,
            files_changed: None,
            lines_added: None,
            lines_removed: None,
        }
    }

    /// Create a PR record with issue linkage.
    pub fn for_issue(
        pr_url: impl Into<String>,
        github_repo: impl Into<String>,
        pr_number: i64,
        issue_source: impl Into<String>,
        issue_id: impl Into<String>,
    ) -> Self {
        let mut record = Self::new(pr_url, github_repo, pr_number);
        record.issue_source = Some(issue_source.into());
        record.issue_id = Some(issue_id.into());
        record
    }
}

/// Aggregate PR analytics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PrAnalytics {
    /// Total number of PRs.
    pub total: i64,
    /// Number of open PRs.
    pub open: i64,
    /// Number of merged PRs.
    pub merged: i64,
    /// Number of closed PRs (without merge).
    pub closed: i64,

    /// Average time to first review in minutes.
    pub avg_time_to_first_review_mins: Option<f64>,
    /// Average time to merge in minutes.
    pub avg_time_to_merge_mins: Option<f64>,
    /// Average review cycles per PR.
    pub avg_review_cycles: Option<f64>,

    /// Merge rate (merged / (merged + closed)).
    pub merge_rate: Option<f64>,

    /// PRs by repository.
    pub by_repo: HashMap<String, i64>,
}

/// Issue embedding for similarity search.
#[derive(Debug, Clone)]
pub struct IssueEmbedding {
    /// Database ID.
    pub id: i64,
    /// Source service (e.g., 'linear', 'sentry').
    pub source: String,
    /// Issue ID from the source.
    pub issue_id: String,
    /// Human-readable issue ID.
    pub short_id: Option<String>,
    /// Issue title.
    pub title: Option<String>,
    /// The embedding vector (serialized float32).
    pub embedding: Vec<f32>,
    /// Model used to generate the embedding.
    pub embedding_model: Option<String>,
    /// When the embedding was created.
    pub created_at: DateTime<Utc>,
}

impl IssueEmbedding {
    /// Create a new issue embedding.
    pub fn new(
        source: impl Into<String>,
        issue_id: impl Into<String>,
        embedding: Vec<f32>,
    ) -> Self {
        Self {
            id: 0,
            source: source.into(),
            issue_id: issue_id.into(),
            short_id: None,
            title: None,
            embedding,
            embedding_model: None,
            created_at: Utc::now(),
        }
    }
}

/// Error pattern for recurring error analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPattern {
    /// Database ID.
    pub id: i64,
    /// Hash of the normalized error (for deduplication).
    pub pattern_hash: String,
    /// Error type (build_failure, test_failure, timeout, claude_error).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// The error message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// When first seen.
    pub first_seen: DateTime<Utc>,
    /// When last seen.
    pub last_seen: DateTime<Utc>,
    /// How many times this pattern occurred.
    pub occurrence_count: i32,
    /// JSON array of sources that hit this error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,
    /// JSON array of example issue IDs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example_issue_ids: Option<Vec<String>>,
    /// Learned hints for fixing this error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_hints: Option<String>,
}

impl ErrorPattern {
    /// Create a new error pattern.
    pub fn new(pattern_hash: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: 0,
            pattern_hash: pattern_hash.into(),
            error_type: None,
            error_message: None,
            first_seen: now,
            last_seen: now,
            occurrence_count: 1,
            sources: None,
            example_issue_ids: None,
            resolution_hints: None,
        }
    }
}

/// Processing metric for time-series data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingMetric {
    /// Database ID.
    pub id: i64,
    /// When the metric was recorded.
    pub timestamp: DateTime<Utc>,
    /// Metric name (queue_depth, processing_time, success_rate, etc.).
    pub metric_name: String,
    /// Metric value.
    pub metric_value: f64,
    /// Optional source for per-source metrics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Additional dimensions as JSON.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<serde_json::Value>,
}

impl ProcessingMetric {
    /// Create a new processing metric.
    pub fn new(metric_name: impl Into<String>, metric_value: f64) -> Self {
        Self {
            id: 0,
            timestamp: Utc::now(),
            metric_name: metric_name.into(),
            metric_value,
            source: None,
            tags: None,
        }
    }

    /// Set the source for this metric.
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set tags for this metric.
    pub fn with_tags(mut self, tags: serde_json::Value) -> Self {
        self.tags = Some(tags);
        self
    }
}

/// Prompt experiment for A/B testing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptExperiment {
    /// Database ID.
    pub id: i64,
    /// Experiment name.
    pub experiment_name: String,
    /// Variant (control, variant_a, etc.).
    pub variant: String,
    /// The prompt template.
    pub prompt_template: String,
    /// Hash of the prompt.
    pub prompt_hash: String,
    /// When the experiment was created.
    pub created_at: DateTime<Utc>,
    /// Whether this variant is active.
    pub active: bool,
    /// Number of successful outcomes.
    pub success_count: i32,
    /// Number of failed outcomes.
    pub failure_count: i32,
    /// Average time to merge (in hours).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_time_to_merge: Option<f64>,
    /// Average review score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_review_score: Option<f64>,
}

impl PromptExperiment {
    /// Create a new prompt experiment.
    pub fn new(
        experiment_name: impl Into<String>,
        variant: impl Into<String>,
        prompt_template: impl Into<String>,
        prompt_hash: impl Into<String>,
    ) -> Self {
        Self {
            id: 0,
            experiment_name: experiment_name.into(),
            variant: variant.into(),
            prompt_template: prompt_template.into(),
            prompt_hash: prompt_hash.into(),
            created_at: Utc::now(),
            active: true,
            success_count: 0,
            failure_count: 0,
            avg_time_to_merge: None,
            avg_review_score: None,
        }
    }
}

/// Similar issue match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimilarIssue {
    /// Database ID.
    pub id: i64,
    /// The source issue ID.
    pub source_issue_id: String,
    /// The similar issue ID.
    pub similar_issue_id: String,
    /// Similarity score (0.0 to 1.0).
    pub similarity_score: f64,
    /// When the similarity was computed.
    pub computed_at: DateTime<Utc>,
}

impl SimilarIssue {
    /// Create a new similar issue record.
    pub fn new(
        source_issue_id: impl Into<String>,
        similar_issue_id: impl Into<String>,
        similarity_score: f64,
    ) -> Self {
        Self {
            id: 0,
            source_issue_id: source_issue_id.into(),
            similar_issue_id: similar_issue_id.into(),
            similarity_score,
            computed_at: Utc::now(),
        }
    }
}

/// Analytics summary statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalyticsSummary {
    /// Overall success rate (0.0 to 1.0).
    pub success_rate: f64,
    /// Total issues processed.
    pub total_processed: i64,
    /// Total successful fixes.
    pub total_successful: i64,
    /// Total merged PRs.
    pub total_merged: i64,
    /// Average processing time in seconds.
    pub avg_processing_time_secs: Option<f64>,
    /// Average time to merge in hours.
    pub avg_time_to_merge_hours: Option<f64>,
    /// Most common error type.
    pub most_common_error: Option<String>,
    /// Success rate by source.
    pub success_rate_by_source: HashMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_creation() {
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        assert_eq!(issue.id, "123");
        assert_eq!(issue.short_id, "PROJ-123");
        assert_eq!(issue.source, "linear");
        assert_eq!(issue.priority, IssuePriority::None);
        assert_eq!(issue.status, IssueStatus::Open);
    }

    #[test]
    fn test_issue_metadata() {
        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("count", 42i64);
        assert_eq!(issue.get_metadata::<i64>("count"), Some(42));
    }

    #[test]
    fn test_match_result() {
        let matched = MatchResult::matched("Matches criteria", MatchPriority::High);
        assert!(matched.matches);
        assert_eq!(matched.priority, MatchPriority::High);

        let not_matched = MatchResult::not_matched("Does not match");
        assert!(!not_matched.matches);
    }

    #[test]
    fn test_issue_serialization() {
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        let json = serde_json::to_string(&issue).unwrap();
        let deserialized: Issue = serde_json::from_str(&json).unwrap();
        assert_eq!(issue.id, deserialized.id);
        assert_eq!(issue.short_id, deserialized.short_id);
    }

    #[test]
    fn test_priority_ordering() {
        assert!(IssuePriority::Critical > IssuePriority::High);
        assert!(IssuePriority::High > IssuePriority::Medium);
        assert!(IssuePriority::Medium > IssuePriority::Low);
        assert!(IssuePriority::Low > IssuePriority::None);
    }

    #[test]
    fn test_fix_attempt_status_parsing() {
        assert_eq!(
            "pending".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::Pending
        );
        assert_eq!(
            "SUCCESS".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::Success
        );
        assert_eq!(
            "Failed".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::Failed
        );
        assert_eq!(
            "merged".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::Merged
        );
        assert_eq!(
            "CLOSED".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::Closed
        );
        assert_eq!(
            "cannot_fix".parse::<FixAttemptStatus>().unwrap(),
            FixAttemptStatus::CannotFix
        );
    }

    #[test]
    fn test_fix_attempt_status_parsing_invalid() {
        assert!("invalid".parse::<FixAttemptStatus>().is_err());
        assert!("".parse::<FixAttemptStatus>().is_err());
    }

    #[test]
    fn test_fix_attempt_status_display() {
        assert_eq!(FixAttemptStatus::Pending.to_string(), "pending");
        assert_eq!(FixAttemptStatus::Success.to_string(), "success");
        assert_eq!(FixAttemptStatus::Failed.to_string(), "failed");
        assert_eq!(FixAttemptStatus::Merged.to_string(), "merged");
        assert_eq!(FixAttemptStatus::Closed.to_string(), "closed");
        assert_eq!(FixAttemptStatus::CannotFix.to_string(), "cannot_fix");
    }

    #[test]
    fn test_issue_priority_display() {
        assert_eq!(IssuePriority::None.to_string(), "none");
        assert_eq!(IssuePriority::Low.to_string(), "low");
        assert_eq!(IssuePriority::Medium.to_string(), "medium");
        assert_eq!(IssuePriority::High.to_string(), "high");
        assert_eq!(IssuePriority::Critical.to_string(), "critical");
    }

    #[test]
    fn test_issue_status_display() {
        assert_eq!(IssueStatus::Open.to_string(), "open");
        assert_eq!(IssueStatus::InProgress.to_string(), "in_progress");
        assert_eq!(IssueStatus::Resolved.to_string(), "resolved");
        assert_eq!(IssueStatus::Ignored.to_string(), "ignored");
    }

    #[test]
    fn test_issue_priority_default() {
        assert_eq!(IssuePriority::default(), IssuePriority::None);
    }

    #[test]
    fn test_issue_status_default() {
        assert_eq!(IssueStatus::default(), IssueStatus::Open);
    }

    #[test]
    fn test_match_priority_default() {
        assert_eq!(MatchPriority::default(), MatchPriority::Normal);
    }

    #[test]
    fn test_match_priority_ordering() {
        assert!(MatchPriority::Urgent > MatchPriority::High);
        assert!(MatchPriority::High > MatchPriority::Normal);
        assert!(MatchPriority::Normal > MatchPriority::Low);
    }

    #[test]
    fn test_issue_with_description() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.description = Some("Description text".to_string());
        assert_eq!(issue.description.as_deref(), Some("Description text"));
    }

    #[test]
    fn test_issue_metadata_string() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("key", "value");
        assert_eq!(
            issue.get_metadata::<String>("key"),
            Some("value".to_string())
        );
    }

    #[test]
    fn test_issue_metadata_bool() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("flag", true);
        assert_eq!(issue.get_metadata::<bool>("flag"), Some(true));
    }

    #[test]
    fn test_issue_metadata_vec() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("tags", vec!["a", "b", "c"]);
        assert_eq!(
            issue.get_metadata::<Vec<String>>("tags"),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn test_issue_metadata_missing_key() {
        let issue = Issue::new("1", "T-1", "Title", "url", "src");
        assert_eq!(issue.get_metadata::<String>("nonexistent"), None);
    }

    #[test]
    fn test_issue_metadata_wrong_type() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("number", 42i64);
        // Trying to get a number as a string should fail
        assert_eq!(issue.get_metadata::<String>("number"), None);
    }

    #[test]
    fn test_claude_result_success() {
        let result = ClaudeResult {
            success: true,
            output: "PR created".to_string(),
            pr_url: Some("https://github.com/test/pr/1".to_string()),
            error: None,
        };
        assert!(result.success);
        assert!(result.pr_url.is_some());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_claude_result_failure() {
        let result = ClaudeResult {
            success: false,
            output: "".to_string(),
            pr_url: None,
            error: Some("Build failed".to_string()),
        };
        assert!(!result.success);
        assert!(result.pr_url.is_none());
        assert!(result.error.is_some());
    }

    #[test]
    fn test_fix_attempt_stats_default() {
        let stats = FixAttemptStats::default();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.success, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.merged, 0);
        assert_eq!(stats.closed, 0);
        assert_eq!(stats.cannot_fix, 0);
        assert!(stats.by_source.is_empty());
    }

    #[test]
    fn test_source_stats_default() {
        let stats = SourceStats::default();
        assert_eq!(stats.total, 0);
        assert_eq!(stats.success, 0);
        assert_eq!(stats.failed, 0);
        assert_eq!(stats.merged, 0);
        assert_eq!(stats.closed, 0);
        assert_eq!(stats.cannot_fix, 0);
    }

    #[test]
    fn test_issue_priority_serde() {
        let priority = IssuePriority::High;
        let json = serde_json::to_string(&priority).unwrap();
        assert_eq!(json, "\"high\"");
        let parsed: IssuePriority = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, IssuePriority::High);
    }

    #[test]
    fn test_issue_status_serde() {
        let status = IssueStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"in_progress\"");
        let parsed: IssueStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, IssueStatus::InProgress);
    }

    #[test]
    fn test_fix_attempt_status_serde() {
        let status = FixAttemptStatus::CannotFix;
        let json = serde_json::to_string(&status).unwrap();
        // Note: rename_all = "lowercase" makes this "cannotfix"
        assert_eq!(json, "\"cannotfix\"");
        let parsed: FixAttemptStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FixAttemptStatus::CannotFix);
    }

    #[test]
    fn test_match_result_serde() {
        let result = MatchResult::matched("test", MatchPriority::High);
        let json = serde_json::to_string(&result).unwrap();
        let parsed: MatchResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.matches, result.matches);
        assert_eq!(parsed.reason, result.reason);
        assert_eq!(parsed.priority, result.priority);
    }

    #[test]
    fn test_issue_full_serde() {
        let mut issue = Issue::new("id", "short", "title", "url", "source");
        issue.description = Some("desc".to_string());
        issue.priority = IssuePriority::Critical;
        issue.status = IssueStatus::InProgress;
        issue.set_metadata("key", "value");
        issue.created_at = Some(chrono::Utc::now());

        let json = serde_json::to_string(&issue).unwrap();
        let parsed: Issue = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, issue.id);
        assert_eq!(parsed.short_id, issue.short_id);
        assert_eq!(parsed.title, issue.title);
        assert_eq!(parsed.description, issue.description);
        assert_eq!(parsed.url, issue.url);
        assert_eq!(parsed.source, issue.source);
        assert_eq!(parsed.priority, issue.priority);
        assert_eq!(parsed.status, issue.status);
    }

    #[test]
    fn test_fix_attempt_created_time() {
        let attempt = FixAttempt {
            id: 1,
            source: "linear".to_string(),
            issue_id: "123".to_string(),
            short_id: "LIN-123".to_string(),
            status: FixAttemptStatus::Pending,
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            error_message: None,
            attempted_at: chrono::Utc::now(),
            resolved_at: None,
            merged_at: None,
            retry_count: 0,
            last_retry_at: None,
        };

        // Verify fields
        assert_eq!(attempt.id, 1);
        assert_eq!(attempt.source, "linear");
        assert_eq!(attempt.retry_count, 0);
        assert!(attempt.resolved_at.is_none());
    }

    #[test]
    fn test_fix_attempt_with_pr() {
        let attempt = FixAttempt {
            id: 1,
            source: "linear".to_string(),
            issue_id: "123".to_string(),
            short_id: "LIN-123".to_string(),
            status: FixAttemptStatus::Success,
            pr_url: Some("https://github.com/org/repo/pull/42".to_string()),
            github_repo: Some("org/repo".to_string()),
            github_pr_number: Some(42),
            error_message: None,
            attempted_at: chrono::Utc::now(),
            resolved_at: None,
            merged_at: None,
            retry_count: 0,
            last_retry_at: None,
        };

        assert_eq!(
            attempt.pr_url,
            Some("https://github.com/org/repo/pull/42".to_string())
        );
        assert_eq!(attempt.github_repo, Some("org/repo".to_string()));
        assert_eq!(attempt.github_pr_number, Some(42));
    }

    #[test]
    fn test_fix_attempt_status_all_variants() {
        assert_eq!(FixAttemptStatus::Pending.to_string(), "pending");
        assert_eq!(FixAttemptStatus::Success.to_string(), "success");
        assert_eq!(FixAttemptStatus::Failed.to_string(), "failed");
        assert_eq!(FixAttemptStatus::Merged.to_string(), "merged");
        assert_eq!(FixAttemptStatus::Closed.to_string(), "closed");
        assert_eq!(FixAttemptStatus::CannotFix.to_string(), "cannot_fix");
    }

    #[test]
    fn test_fix_attempt_status_serde_all_variants() {
        let statuses = vec![
            FixAttemptStatus::Pending,
            FixAttemptStatus::Success,
            FixAttemptStatus::Failed,
            FixAttemptStatus::Merged,
            FixAttemptStatus::Closed,
            FixAttemptStatus::CannotFix,
        ];

        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: FixAttemptStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_issue_priority_serde_all_variants() {
        let priorities = vec![
            IssuePriority::None,
            IssuePriority::Low,
            IssuePriority::Medium,
            IssuePriority::High,
            IssuePriority::Critical,
        ];

        for priority in priorities {
            let json = serde_json::to_string(&priority).unwrap();
            let parsed: IssuePriority = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, priority);
        }
    }

    #[test]
    fn test_issue_status_serde_all_variants() {
        let statuses = vec![
            IssueStatus::Open,
            IssueStatus::InProgress,
            IssueStatus::Resolved,
            IssueStatus::Ignored,
        ];

        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: IssueStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_match_priority_serde_all_variants() {
        let priorities = vec![
            MatchPriority::Low,
            MatchPriority::Normal,
            MatchPriority::High,
            MatchPriority::Urgent,
        ];

        for priority in priorities {
            let json = serde_json::to_string(&priority).unwrap();
            let parsed: MatchPriority = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, priority);
        }
    }

    #[test]
    fn test_issue_metadata_number() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("count", 42i64);
        assert_eq!(issue.get_metadata::<i64>("count"), Some(42));
    }

    #[test]
    fn test_issue_metadata_nested_object() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("nested", serde_json::json!({"a": 1, "b": "test"}));

        let nested: serde_json::Value = issue.get_metadata("nested").unwrap();
        assert_eq!(nested["a"], 1);
        assert_eq!(nested["b"], "test");
    }

    #[test]
    fn test_issue_clone() {
        let mut original = Issue::new("id", "short", "title", "url", "source");
        original.description = Some("desc".to_string());
        original.priority = IssuePriority::High;
        original.set_metadata("key", "value");

        let cloned = original.clone();

        assert_eq!(cloned.id, original.id);
        assert_eq!(cloned.description, original.description);
        assert_eq!(cloned.priority, original.priority);
    }

    #[test]
    fn test_match_result_with_empty_reason() {
        let result = MatchResult::not_matched("");
        assert!(!result.matches);
        assert!(result.reason.is_empty());
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_fix_attempt_stats_with_data() {
        let mut stats = FixAttemptStats::default();
        stats.total = 100;
        stats.pending = 10;
        stats.success = 50;
        stats.failed = 20;
        stats.merged = 15;
        stats.closed = 3;
        stats.cannot_fix = 2;

        assert_eq!(stats.total, 100);
        assert_eq!(
            stats.pending
                + stats.success
                + stats.failed
                + stats.merged
                + stats.closed
                + stats.cannot_fix,
            100
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn test_source_stats_with_data() {
        let mut stats = SourceStats::default();
        stats.total = 50;
        stats.success = 30;
        stats.failed = 10;
        stats.merged = 8;
        stats.closed = 2;
        stats.cannot_fix = 0;

        assert_eq!(stats.total, 50);
    }

    #[test]
    fn test_claude_result_empty_output() {
        let result = ClaudeResult {
            success: false,
            output: "".to_string(),
            pr_url: None,
            error: Some("No output".to_string()),
        };

        assert!(result.output.is_empty());
        assert!(result.error.is_some());
    }

    #[test]
    fn test_issue_with_updated_at() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        assert!(issue.updated_at.is_none());

        issue.updated_at = Some(chrono::Utc::now());
        assert!(issue.updated_at.is_some());
    }

    #[test]
    fn test_match_result_debug_format() {
        let result = MatchResult::matched("Test reason", MatchPriority::High);
        let debug_str = format!("{:?}", result);
        assert!(debug_str.contains("matches"));
        assert!(debug_str.contains("priority"));
    }

    #[test]
    fn test_issue_debug_format() {
        let issue = Issue::new("1", "T-1", "Title", "url", "src");
        let debug_str = format!("{:?}", issue);
        assert!(debug_str.contains("Issue"));
    }

    // Tests for validate_issue_id

    #[test]
    fn test_validate_issue_id_valid() {
        assert!(validate_issue_id("PROJ-123").is_ok());
        assert!(validate_issue_id("abc123").is_ok());
        assert!(validate_issue_id("simple_id").is_ok());
        assert!(validate_issue_id("ID-WITH-DASHES").is_ok());
        assert!(validate_issue_id("123456").is_ok());
        assert!(validate_issue_id("a").is_ok());
    }

    #[test]
    fn test_validate_issue_id_empty() {
        let result = validate_issue_id("");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_validate_issue_id_too_long() {
        let long_id = "x".repeat(MAX_ISSUE_ID_LENGTH + 1);
        let result = validate_issue_id(&long_id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("maximum length"));
    }

    #[test]
    fn test_validate_issue_id_at_max_length() {
        let max_id = "x".repeat(MAX_ISSUE_ID_LENGTH);
        assert!(validate_issue_id(&max_id).is_ok());
    }

    #[test]
    fn test_validate_issue_id_path_traversal() {
        assert!(validate_issue_id("..").is_err());
        assert!(validate_issue_id("../etc/passwd").is_err());
        assert!(validate_issue_id("foo..bar").is_err());
        assert!(validate_issue_id("a/../b").is_err());
    }

    #[test]
    fn test_validate_issue_id_forward_slash() {
        assert!(validate_issue_id("a/b").is_err());
        assert!(validate_issue_id("/leading").is_err());
        assert!(validate_issue_id("trailing/").is_err());
        assert!(validate_issue_id("path/to/something").is_err());
    }

    #[test]
    fn test_validate_issue_id_backslash() {
        assert!(validate_issue_id("a\\b").is_err());
        assert!(validate_issue_id("\\leading").is_err());
        assert!(validate_issue_id("windows\\path").is_err());
    }

    #[test]
    fn test_validate_issue_id_null_byte() {
        assert!(validate_issue_id("foo\0bar").is_err());
        assert!(validate_issue_id("\0").is_err());
    }

    #[test]
    fn test_validate_issue_id_unicode() {
        // Unicode should be allowed (for international issue IDs)
        assert!(validate_issue_id("项目-123").is_ok());
        assert!(validate_issue_id("задача-456").is_ok());
    }

    #[test]
    fn test_validate_issue_id_special_chars() {
        // These special chars should be allowed
        assert!(validate_issue_id("ID_with_underscore").is_ok());
        assert!(validate_issue_id("ID-with-dash").is_ok());
        assert!(validate_issue_id("ID.with.dot").is_ok());
        assert!(validate_issue_id("ID:with:colon").is_ok());
    }
}

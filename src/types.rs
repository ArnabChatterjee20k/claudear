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
    /// Structured blocking question emitted by Claude when it requires human input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocking_question: Option<BlockingQuestion>,
    /// Q&A knowledge IDs used while preparing this run.
    #[serde(default)]
    pub used_qa_ids: Vec<i64>,
}

/// Structured blocking question emitted by Claude.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockingQuestion {
    /// The actual question needing a human answer.
    pub question: String,
    /// Optional context Claude includes to help the responder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Optional options Claude proposes.
    #[serde(default)]
    pub options: Vec<String>,
    /// Optional explanation of why the question is required.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// Ask request used by notification channels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskRequest {
    /// Correlation ID for cross-channel dedupe and reply matching.
    pub correlation_id: String,
    /// Source service name (e.g. linear, sentry).
    pub source: String,
    /// Optional target repository for scoped reuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Source issue ID.
    pub issue_id: String,
    /// Human-readable issue key.
    pub short_id: String,
    /// Question payload.
    pub question: BlockingQuestion,
    /// Ask timestamp.
    pub asked_at: DateTime<Utc>,
    /// Optional desired Discord responder ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_discord_id: Option<String>,
    /// Optional desired email responder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_email: Option<String>,
    /// Optional desired Slack responder ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_slack_id: Option<String>,
}

/// Delivery metadata for an ask message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskDelivery {
    /// Channel name (discord/email/sms/push).
    pub channel: String,
    /// Channel-specific target identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Channel-specific message/thread ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// Reply captured from a notification channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskReply {
    /// Correlation ID to link with ask.
    pub correlation_id: String,
    /// Channel where the reply was received.
    pub channel: String,
    /// Responder identity (channel user ID or email).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responder: Option<String>,
    /// Raw reply text.
    pub answer: String,
    /// Reply timestamp.
    pub replied_at: DateTime<Utc>,
}

/// Status of a fix attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FixAttemptStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "success")]
    Success,
    #[serde(rename = "failed")]
    Failed,
    /// PR was merged and issue was resolved.
    #[serde(rename = "merged")]
    Merged,
    /// PR was closed without merging.
    #[serde(rename = "closed")]
    Closed,
    /// Max retries reached, issue cannot be automatically fixed.
    #[serde(rename = "cannot_fix")]
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
    /// Labels from the issue (for bug detection).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issue_labels: Vec<String>,
    /// Parent attempt ID for cascade chains. NULL for root attempts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_attempt_id: Option<i64>,
    /// Target repository for cascade attempts (e.g., "appwrite/appwrite").
    /// NULL for root attempts (original issue fix).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cascade_repo: Option<String>,
}

impl FixAttempt {
    /// Check if this fix attempt is for a bug (based on labels).
    ///
    /// Returns true if:
    /// - Source is "sentry" (all Sentry issues are bugs)
    /// - Issue has a label indicating it's a bug (e.g., "bug", "defect", "error")
    pub fn is_bug(&self) -> bool {
        // Sentry issues are always bugs
        if self.source == "sentry" {
            return true;
        }

        // Check for common bug labels
        const BUG_LABELS: &[&str] = &[
            "bug",
            "defect",
            "error",
            "fix",
            "hotfix",
            "regression",
            "issue",
            "problem",
            "incident",
            "crash",
            "broken",
        ];

        self.issue_labels.iter().any(|label| {
            let lower = label.to_lowercase();
            BUG_LABELS.iter().any(|bug_label| lower.contains(bug_label))
        })
    }
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
    /// Absolute path to the captured stdout log file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_log_path: Option<String>,
    /// Absolute path to the captured stderr log file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_log_path: Option<String>,
    /// Absolute path to the captured execution event log file (JSONL).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_log_path: Option<String>,
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
            stdout_log_path: None,
            stderr_log_path: None,
            event_log_path: None,
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
    /// Issue description or error message.
    pub description: Option<String>,
    /// URL to view the issue in its source.
    pub url: Option<String>,
    /// Priority level (none, low, medium, high, critical).
    pub priority: Option<String>,
    /// Current status (open, in_progress, resolved, ignored).
    pub status: Option<String>,
    /// Labels as JSON array text.
    pub labels: Option<String>,
    /// The embedding vector (serialized float32). None if issue stored without embedding.
    pub embedding: Option<Vec<f32>>,
    /// Model used to generate the embedding.
    pub embedding_model: Option<String>,
    /// When the embedding was created.
    pub created_at: DateTime<Utc>,
    /// When the issue was last updated.
    pub updated_at: Option<DateTime<Utc>>,
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
            description: None,
            url: None,
            priority: None,
            status: None,
            labels: None,
            embedding: Some(embedding),
            embedding_model: None,
            created_at: Utc::now(),
            updated_at: None,
        }
    }

    /// Create an IssueEmbedding from an Issue, storing content fields without an embedding.
    pub fn from_issue(issue: &Issue) -> Self {
        let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();
        let labels_json = if labels.is_empty() {
            None
        } else {
            serde_json::to_string(&labels).ok()
        };
        Self {
            id: 0,
            source: issue.source.clone(),
            issue_id: issue.id.clone(),
            short_id: Some(issue.short_id.clone()),
            title: Some(issue.title.clone()),
            description: issue.description.clone(),
            url: Some(issue.url.clone()),
            priority: Some(issue.priority.to_string()),
            status: Some(issue.status.to_string()),
            labels: labels_json,
            embedding: None,
            embedding_model: None,
            created_at: issue.created_at.unwrap_or_else(Utc::now),
            updated_at: issue.updated_at,
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

// ============================================================
// Prioritisation types
// ============================================================

/// Composite severity score produced by the prioritisation engine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SeverityScore {
    /// Final weighted score (higher = more urgent).
    pub score: f64,
    /// Contribution from issue/match priority (0.0-1.0).
    pub severity_component: f64,
    /// Contribution from event frequency signals (0.0-1.0).
    pub frequency_component: f64,
    /// Contribution from regression risk signals (0.0-1.0).
    pub regression_component: f64,
    /// Contribution from blast radius classification (0.0-1.0).
    pub blast_radius_component: f64,
    /// Boost from belonging to a content cluster (0.0 or 1.0).
    pub cluster_boost: f64,
}

/// Blast radius classification for an issue.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum BlastRadius {
    Cosmetic,
    Test,
    Peripheral,
    #[default]
    Core,
    Infrastructure,
    Critical,
}

impl std::fmt::Display for BlastRadius {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cosmetic => write!(f, "cosmetic"),
            Self::Test => write!(f, "test"),
            Self::Peripheral => write!(f, "peripheral"),
            Self::Core => write!(f, "core"),
            Self::Infrastructure => write!(f, "infrastructure"),
            Self::Critical => write!(f, "critical"),
        }
    }
}

/// A cluster of content-similar issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentCluster {
    /// Database ID (0 before storage).
    pub id: i64,
    /// Key identifying the cluster (e.g. "TypeError::app.main").
    pub cluster_key: String,
    /// Source service name.
    pub source: String,
    /// Issue ID of the representative (highest-scored) issue.
    pub representative_issue_id: String,
    /// All issue IDs in the cluster.
    pub issue_ids: Vec<String>,
    /// Shared error type (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_type: Option<String>,
    /// Shared culprit (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub culprit: Option<String>,
    /// Average pairwise similarity within the cluster.
    pub avg_similarity: f64,
    /// Status: "active" or "resolved".
    pub status: String,
    /// When the cluster was created.
    pub created_at: DateTime<Utc>,
}

/// A user-configured suppression rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionRule {
    /// Human-readable rule name.
    pub name: String,
    /// Which issue field to match against.
    pub field: SuppressionField,
    /// Pattern to match.
    pub pattern: String,
    /// How to match the pattern.
    #[serde(default)]
    pub match_mode: SuppressionMatchMode,
    /// Restrict this rule to specific sources (empty = all sources).
    #[serde(default)]
    pub sources: Vec<String>,
    /// Human-readable reason for suppression.
    #[serde(default)]
    pub reason: String,
}

/// Which issue field a suppression rule matches against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionField {
    Title,
    Description,
    Source,
    Culprit,
    Filename,
    ErrorType,
    Project,
    Labels,
    Metadata(String),
}

/// How a suppression pattern is matched.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionMatchMode {
    #[default]
    Contains,
    Exact,
    Regex,
}

/// Result of evaluating suppression rules against an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionResult {
    /// Whether the issue was suppressed.
    pub suppressed: bool,
    /// Name of the matched rule (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<String>,
    /// Reason from the matched rule (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An issue after scoring and classification by the prioritisation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrioritisedIssue {
    /// The original issue.
    pub issue: Issue,
    /// Criteria match result.
    pub match_result: MatchResult,
    /// Composite severity score.
    pub severity_score: SeverityScore,
    /// Blast radius classification.
    pub blast_radius: BlastRadius,
    /// Cluster key if the issue belongs to a content cluster.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_key: Option<String>,
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

/// Stored Q&A knowledge entry used for semantic reuse.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaKnowledgeEntry {
    pub id: i64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub issue_id: String,
    pub short_id: String,
    pub question_text: String,
    pub question_norm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question_embedding: Option<Vec<f32>>,
    pub answer_text: String,
    pub answer_norm: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_embedding: Option<Vec<f32>>,
    pub channel: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub responder: Option<String>,
    pub correlation_id: String,
    pub asked_at: DateTime<Utc>,
    pub answered_at: DateTime<Utc>,
    pub success_count: i64,
    pub failure_count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

/// Similar Q&A match used during retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaMatch {
    pub entry: QaKnowledgeEntry,
    pub semantic_similarity: f64,
    pub historical_success_rate: f64,
    pub final_score: f64,
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

/// Status of a regression watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RegressionWatchStatus {
    /// Waiting for the fix to be included in a release.
    #[default]
    AwaitingRelease,
    /// Release detected, actively monitoring for regressions.
    Monitoring,
    /// No regression detected after monitoring period, issue resolved.
    Resolved,
    /// Regression detected, fix failed.
    Regressed,
}

impl std::fmt::Display for RegressionWatchStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AwaitingRelease => write!(f, "awaiting_release"),
            Self::Monitoring => write!(f, "monitoring"),
            Self::Resolved => write!(f, "resolved"),
            Self::Regressed => write!(f, "regressed"),
        }
    }
}

impl std::str::FromStr for RegressionWatchStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "awaiting_release" => Ok(Self::AwaitingRelease),
            "monitoring" => Ok(Self::Monitoring),
            "resolved" => Ok(Self::Resolved),
            "regressed" => Ok(Self::Regressed),
            _ => Err(format!("Unknown regression watch status: {}", s)),
        }
    }
}

/// Type of issue being watched for regression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueType {
    /// Issue originated from Sentry.
    SentryIssue,
    /// Issue from Linear marked as a bug.
    LinearBug,
    /// Issue from GitLab.
    GitLabIssue,
    /// Issue from Jira.
    JiraIssue,
}

impl IssueType {
    /// Get the source name for this issue type (used for retry lookups).
    pub fn source_name(&self) -> &'static str {
        match self {
            Self::SentryIssue => "sentry",
            Self::LinearBug => "linear",
            Self::GitLabIssue => "gitlab",
            Self::JiraIssue => "jira",
        }
    }
}

impl std::fmt::Display for IssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SentryIssue => write!(f, "sentry_issue"),
            Self::LinearBug => write!(f, "linear_bug"),
            Self::GitLabIssue => write!(f, "gitlab_issue"),
            Self::JiraIssue => write!(f, "jira_issue"),
        }
    }
}

impl std::str::FromStr for IssueType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "sentry_issue" => Ok(Self::SentryIssue),
            "linear_bug" => Ok(Self::LinearBug),
            "gitlab_issue" => Ok(Self::GitLabIssue),
            "jira_issue" => Ok(Self::JiraIssue),
            _ => Err(format!("Unknown issue type: {}", s)),
        }
    }
}

/// A watch for regression after a bug fix is merged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionWatch {
    /// Database ID.
    pub id: i64,
    /// Type of issue (SentryIssue or LinearBug).
    pub issue_type: IssueType,
    /// Issue ID from the source.
    pub issue_id: String,
    /// Reference to the fix attempt.
    pub fix_attempt_id: i64,
    /// Current status of the watch.
    pub status: RegressionWatchStatus,
    /// When the PR was merged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_merged_at: Option<DateTime<Utc>>,
    /// When regression monitoring started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitoring_started_at: Option<DateTime<Utc>>,
    /// When the issue was resolved (after 24h of no regression).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    /// When a regression was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regressed_at: Option<DateTime<Utc>>,
    /// When the watch was created.
    pub created_at: DateTime<Utc>,
}

impl RegressionWatch {
    /// Create a new regression watch.
    pub fn new(issue_type: IssueType, issue_id: impl Into<String>, fix_attempt_id: i64) -> Self {
        Self {
            id: 0,
            issue_type,
            issue_id: issue_id.into(),
            fix_attempt_id,
            status: RegressionWatchStatus::AwaitingRelease,
            pr_merged_at: None,
            monitoring_started_at: None,
            resolved_at: None,
            regressed_at: None,
            created_at: Utc::now(),
        }
    }
}

/// Tracking of a release that may contain a fix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseTracking {
    /// Database ID.
    pub id: i64,
    /// Reference to the regression watch.
    pub regression_watch_id: i64,
    /// Release version/tag.
    pub release_version: String,
    /// Commit SHA of the release.
    pub release_commit: String,
    /// When the release was detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
    /// When this tracking entry was created.
    pub created_at: DateTime<Utc>,
}

impl ReleaseTracking {
    /// Create a new release tracking entry.
    pub fn new(
        regression_watch_id: i64,
        release_version: impl Into<String>,
        release_commit: impl Into<String>,
    ) -> Self {
        Self {
            id: 0,
            regression_watch_id,
            release_version: release_version.into(),
            release_commit: release_commit.into(),
            released_at: Some(Utc::now()),
            created_at: Utc::now(),
        }
    }
}

/// A single regression check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionCheck {
    /// Database ID.
    pub id: i64,
    /// Reference to the regression watch.
    pub regression_watch_id: i64,
    /// Whether the issue still exists (regression detected).
    pub issue_still_exists: bool,
    /// When the check was performed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    /// Detailed findings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_details: Option<String>,
    /// When this check was created.
    pub created_at: DateTime<Utc>,
}

impl RegressionCheck {
    /// Create a new regression check.
    pub fn new(regression_watch_id: i64, issue_still_exists: bool) -> Self {
        Self {
            id: 0,
            regression_watch_id,
            issue_still_exists,
            checked_at: Some(Utc::now()),
            check_details: None,
            created_at: Utc::now(),
        }
    }
}

// ============================================================
// Continuous Learning Types
// ============================================================

/// Category of file changes in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeCategory {
    Tests,
    Docs,
    Config,
    Dependencies,
    Migrations,
    NewCode,
    Modification,
}

impl std::fmt::Display for ChangeCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tests => write!(f, "tests"),
            Self::Docs => write!(f, "docs"),
            Self::Config => write!(f, "config"),
            Self::Dependencies => write!(f, "dependencies"),
            Self::Migrations => write!(f, "migrations"),
            Self::NewCode => write!(f, "new_code"),
            Self::Modification => write!(f, "modification"),
        }
    }
}

/// Structured analysis of a PR diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffAnalysis {
    pub id: i64,
    pub attempt_id: i64,
    pub pr_url: String,
    pub github_repo: String,
    pub pr_number: i64,
    pub files_changed: Vec<String>,
    pub file_types: HashMap<String, usize>,
    pub change_categories: Vec<ChangeCategory>,
    pub diff_summary: String,
    pub created_at: DateTime<Utc>,
}

/// A standing instruction promoted from repeated Q&A or review patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotedInstruction {
    pub id: i64,
    pub repo: String,
    pub source_type: String,
    pub instruction_text: String,
    pub occurrence_count: i64,
    pub confidence: f64,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-repo accumulated knowledge entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoKnowledge {
    pub id: i64,
    pub repo: String,
    pub knowledge_key: String,
    pub knowledge_value: String,
    pub source_type: String,
    pub confidence: f64,
    pub occurrence_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Category of review feedback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCategory {
    MissingTests,
    StyleIssue,
    WrongApproach,
    Incomplete,
    Security,
    Performance,
    Documentation,
    Other,
}

impl std::fmt::Display for ReviewCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingTests => write!(f, "missing_tests"),
            Self::StyleIssue => write!(f, "style_issue"),
            Self::WrongApproach => write!(f, "wrong_approach"),
            Self::Incomplete => write!(f, "incomplete"),
            Self::Security => write!(f, "security"),
            Self::Performance => write!(f, "performance"),
            Self::Documentation => write!(f, "documentation"),
            Self::Other => write!(f, "other"),
        }
    }
}

impl ReviewCategory {
    pub fn parse(s: &str) -> Self {
        match s {
            "missing_tests" => Self::MissingTests,
            "style_issue" => Self::StyleIssue,
            "wrong_approach" => Self::WrongApproach,
            "incomplete" => Self::Incomplete,
            "security" => Self::Security,
            "performance" => Self::Performance,
            "documentation" => Self::Documentation,
            _ => Self::Other,
        }
    }
}

/// A classified review feedback pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPattern {
    pub id: i64,
    pub github_repo: String,
    pub category: ReviewCategory,
    pub pattern_text: String,
    pub example_comments: Vec<String>,
    pub occurrence_count: i64,
    pub promoted_to_instruction: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// How Claude approached fixing an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyFingerprint {
    pub id: i64,
    pub attempt_id: i64,
    pub files_explored: Vec<String>,
    pub tests_run: i64,
    pub tools_used: HashMap<String, i64>,
    pub fix_approach: String,
    pub strategy_summary: String,
    pub fix_quality_score: Option<f64>,
    pub created_at: DateTime<Utc>,
}

/// Quality score for a fix based on merge velocity and review feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixQualityScore {
    pub score: f64,
    pub merge_speed_component: f64,
    pub review_cycles_component: f64,
    pub approval_component: f64,
}

/// A cluster of correlated issues arriving in a time window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueCluster {
    pub id: i64,
    pub cluster_key: String,
    pub source: String,
    pub issue_ids: Vec<String>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub resolved_by_issue_id: Option<String>,
    pub resolved_by_attempt_id: Option<i64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

/// A member of an issue cluster.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueClusterMember {
    pub cluster_id: i64,
    pub issue_id: String,
    pub arrived_at: DateTime<Utc>,
}

/// Learnings extracted from Claude's execution log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedLearnings {
    pub root_cause: Option<String>,
    pub files_modified: Vec<String>,
    pub strategy_used: Option<String>,
    pub tests_added: bool,
    pub key_decisions: Vec<String>,
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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
        // Serde now matches Display/FromStr: "cannot_fix"
        assert_eq!(json, "\"cannot_fix\"");
        let parsed: FixAttemptStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FixAttemptStatus::CannotFix);
    }

    #[test]
    fn test_fix_attempt_status_serde_roundtrip_all_variants() {
        for status in [
            FixAttemptStatus::Pending,
            FixAttemptStatus::Success,
            FixAttemptStatus::Failed,
            FixAttemptStatus::Merged,
            FixAttemptStatus::Closed,
            FixAttemptStatus::CannotFix,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            // Verify serde serialization matches Display
            assert_eq!(json, format!("\"{}\"", status));
            // Verify round-trip through serde
            let parsed: FixAttemptStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
            // Verify round-trip through FromStr
            let display_str = status.to_string();
            let from_str: FixAttemptStatus = display_str.parse().unwrap();
            assert_eq!(from_str, status);
        }
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
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
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
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
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
            blocking_question: None,
            used_qa_ids: Vec::new(),
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

    #[test]
    fn test_regression_watch_status_display() {
        assert_eq!(
            RegressionWatchStatus::AwaitingRelease.to_string(),
            "awaiting_release"
        );
        assert_eq!(RegressionWatchStatus::Monitoring.to_string(), "monitoring");
        assert_eq!(RegressionWatchStatus::Resolved.to_string(), "resolved");
        assert_eq!(RegressionWatchStatus::Regressed.to_string(), "regressed");
    }

    #[test]
    fn test_regression_watch_status_from_str() {
        assert_eq!(
            "awaiting_release".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::AwaitingRelease
        );
        assert_eq!(
            "monitoring".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::Monitoring
        );
        assert_eq!(
            "resolved".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::Resolved
        );
        assert_eq!(
            "regressed".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::Regressed
        );
    }

    #[test]
    fn test_regression_watch_status_from_str_case_insensitive() {
        assert_eq!(
            "AWAITING_RELEASE".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::AwaitingRelease
        );
        assert_eq!(
            "Monitoring".parse::<RegressionWatchStatus>().unwrap(),
            RegressionWatchStatus::Monitoring
        );
    }

    #[test]
    fn test_regression_watch_status_from_str_invalid() {
        assert!("invalid".parse::<RegressionWatchStatus>().is_err());
        assert!("".parse::<RegressionWatchStatus>().is_err());
    }

    #[test]
    fn test_regression_watch_status_default() {
        assert_eq!(
            RegressionWatchStatus::default(),
            RegressionWatchStatus::AwaitingRelease
        );
    }

    #[test]
    fn test_regression_watch_status_serde() {
        let status = RegressionWatchStatus::Monitoring;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"monitoring\"");
        let parsed: RegressionWatchStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, RegressionWatchStatus::Monitoring);
    }

    #[test]
    fn test_regression_watch_status_serde_all_variants() {
        let statuses = vec![
            RegressionWatchStatus::AwaitingRelease,
            RegressionWatchStatus::Monitoring,
            RegressionWatchStatus::Resolved,
            RegressionWatchStatus::Regressed,
        ];

        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: RegressionWatchStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, status);
        }
    }

    #[test]
    fn test_issue_type_display() {
        assert_eq!(IssueType::SentryIssue.to_string(), "sentry_issue");
        assert_eq!(IssueType::LinearBug.to_string(), "linear_bug");
    }

    #[test]
    fn test_issue_type_from_str() {
        assert_eq!(
            "sentry_issue".parse::<IssueType>().unwrap(),
            IssueType::SentryIssue
        );
        assert_eq!(
            "linear_bug".parse::<IssueType>().unwrap(),
            IssueType::LinearBug
        );
    }

    #[test]
    fn test_issue_type_from_str_case_insensitive() {
        assert_eq!(
            "SENTRY_ISSUE".parse::<IssueType>().unwrap(),
            IssueType::SentryIssue
        );
        assert_eq!(
            "Linear_Bug".parse::<IssueType>().unwrap(),
            IssueType::LinearBug
        );
    }

    #[test]
    fn test_issue_type_from_str_invalid() {
        assert!("invalid".parse::<IssueType>().is_err());
        assert!("github_issue".parse::<IssueType>().is_err());
    }

    #[test]
    fn test_issue_type_serde() {
        let issue_type = IssueType::SentryIssue;
        let json = serde_json::to_string(&issue_type).unwrap();
        assert_eq!(json, "\"sentry_issue\"");
        let parsed: IssueType = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, IssueType::SentryIssue);
    }

    #[test]
    fn test_issue_type_serde_all_variants() {
        let types = vec![IssueType::SentryIssue, IssueType::LinearBug];

        for issue_type in types {
            let json = serde_json::to_string(&issue_type).unwrap();
            let parsed: IssueType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, issue_type);
        }
    }

    #[test]
    fn test_regression_watch_new() {
        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-123", 1);

        assert_eq!(watch.issue_type, IssueType::SentryIssue);
        assert_eq!(watch.issue_id, "sentry-123");
        assert_eq!(watch.fix_attempt_id, 1);
        assert_eq!(watch.status, RegressionWatchStatus::AwaitingRelease);
        assert!(watch.pr_merged_at.is_none());
        assert!(watch.monitoring_started_at.is_none());
        assert!(watch.resolved_at.is_none());
        assert!(watch.regressed_at.is_none());
    }

    #[test]
    fn test_regression_watch_serde() {
        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-456", 2);

        let json = serde_json::to_string(&watch).unwrap();
        let parsed: RegressionWatch = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.issue_type, watch.issue_type);
        assert_eq!(parsed.issue_id, watch.issue_id);
        assert_eq!(parsed.fix_attempt_id, watch.fix_attempt_id);
        assert_eq!(parsed.status, watch.status);
    }

    #[test]
    fn test_regression_watch_clone() {
        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-789", 3);

        let cloned = watch.clone();
        assert_eq!(cloned.issue_type, watch.issue_type);
        assert_eq!(cloned.issue_id, watch.issue_id);
        assert_eq!(cloned.fix_attempt_id, watch.fix_attempt_id);
    }

    #[test]
    fn test_regression_watch_debug() {
        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-123", 1);
        let debug_str = format!("{:?}", watch);
        assert!(debug_str.contains("RegressionWatch"));
    }

    #[test]
    fn test_release_tracking_new() {
        let tracking = ReleaseTracking::new(1, "v1.2.3", "abc123def");

        assert_eq!(tracking.regression_watch_id, 1);
        assert_eq!(tracking.release_version, "v1.2.3");
        assert_eq!(tracking.release_commit, "abc123def");
        assert!(tracking.released_at.is_some());
    }

    #[test]
    fn test_release_tracking_serde() {
        let tracking = ReleaseTracking::new(2, "v2.0.0", "def456abc");

        let json = serde_json::to_string(&tracking).unwrap();
        let parsed: ReleaseTracking = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.regression_watch_id, tracking.regression_watch_id);
        assert_eq!(parsed.release_version, tracking.release_version);
        assert_eq!(parsed.release_commit, tracking.release_commit);
    }

    #[test]
    fn test_release_tracking_clone() {
        let tracking = ReleaseTracking::new(3, "v3.0.0", "xyz789");

        let cloned = tracking.clone();
        assert_eq!(cloned.regression_watch_id, tracking.regression_watch_id);
        assert_eq!(cloned.release_version, tracking.release_version);
    }

    #[test]
    fn test_release_tracking_debug() {
        let tracking = ReleaseTracking::new(1, "v1.0.0", "commit123");
        let debug_str = format!("{:?}", tracking);
        assert!(debug_str.contains("ReleaseTracking"));
    }

    #[test]
    fn test_regression_check_new() {
        let check = RegressionCheck::new(1, false);

        assert_eq!(check.regression_watch_id, 1);
        assert!(!check.issue_still_exists);
        assert!(check.checked_at.is_some());
        assert!(check.check_details.is_none());
    }

    #[test]
    fn test_regression_check_with_details() {
        let mut check = RegressionCheck::new(2, true);
        check.check_details = Some("Issue reoccurred in production".to_string());

        assert!(check.issue_still_exists);
        assert_eq!(
            check.check_details,
            Some("Issue reoccurred in production".to_string())
        );
    }

    #[test]
    fn test_regression_check_serde() {
        let mut check = RegressionCheck::new(3, false);
        check.check_details = Some("No occurrences found".to_string());

        let json = serde_json::to_string(&check).unwrap();
        let parsed: RegressionCheck = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.regression_watch_id, check.regression_watch_id);
        assert_eq!(parsed.issue_still_exists, check.issue_still_exists);
        assert_eq!(parsed.check_details, check.check_details);
    }

    #[test]
    fn test_regression_check_clone() {
        let check = RegressionCheck::new(4, true);

        let cloned = check.clone();
        assert_eq!(cloned.regression_watch_id, check.regression_watch_id);
        assert_eq!(cloned.issue_still_exists, check.issue_still_exists);
    }

    #[test]
    fn test_regression_check_debug() {
        let check = RegressionCheck::new(1, false);
        let debug_str = format!("{:?}", check);
        assert!(debug_str.contains("RegressionCheck"));
    }

    #[test]
    fn test_regression_watch_status_transitions() {
        // Valid transition: AwaitingRelease -> Monitoring
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-1", 1);
        assert_eq!(watch.status, RegressionWatchStatus::AwaitingRelease);

        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(chrono::Utc::now());
        assert_eq!(watch.status, RegressionWatchStatus::Monitoring);

        // Valid transition: Monitoring -> Resolved
        watch.status = RegressionWatchStatus::Resolved;
        watch.resolved_at = Some(chrono::Utc::now());
        assert_eq!(watch.status, RegressionWatchStatus::Resolved);
    }

    #[test]
    fn test_regression_watch_status_regressed_transition() {
        // Valid transition: Monitoring -> Regressed
        let mut watch = RegressionWatch::new(IssueType::LinearBug, "issue-2", 2);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(chrono::Utc::now());

        watch.status = RegressionWatchStatus::Regressed;
        watch.regressed_at = Some(chrono::Utc::now());
        assert_eq!(watch.status, RegressionWatchStatus::Regressed);
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

    // ── Learning types tests ──

    #[test]
    fn test_change_category_display() {
        assert_eq!(ChangeCategory::Tests.to_string(), "tests");
        assert_eq!(ChangeCategory::Docs.to_string(), "docs");
        assert_eq!(ChangeCategory::Config.to_string(), "config");
        assert_eq!(ChangeCategory::Dependencies.to_string(), "dependencies");
        assert_eq!(ChangeCategory::Migrations.to_string(), "migrations");
        assert_eq!(ChangeCategory::NewCode.to_string(), "new_code");
        assert_eq!(ChangeCategory::Modification.to_string(), "modification");
    }

    #[test]
    fn test_review_category_display() {
        assert_eq!(ReviewCategory::MissingTests.to_string(), "missing_tests");
        assert_eq!(ReviewCategory::StyleIssue.to_string(), "style_issue");
        assert_eq!(ReviewCategory::WrongApproach.to_string(), "wrong_approach");
        assert_eq!(ReviewCategory::Incomplete.to_string(), "incomplete");
        assert_eq!(ReviewCategory::Security.to_string(), "security");
        assert_eq!(ReviewCategory::Performance.to_string(), "performance");
        assert_eq!(ReviewCategory::Documentation.to_string(), "documentation");
        assert_eq!(ReviewCategory::Other.to_string(), "other");
    }

    #[test]
    fn test_review_category_parse_roundtrip() {
        let categories = vec![
            ReviewCategory::MissingTests,
            ReviewCategory::StyleIssue,
            ReviewCategory::WrongApproach,
            ReviewCategory::Incomplete,
            ReviewCategory::Security,
            ReviewCategory::Performance,
            ReviewCategory::Documentation,
            ReviewCategory::Other,
        ];
        for cat in categories {
            let display = cat.to_string();
            let parsed = ReviewCategory::parse(&display);
            assert_eq!(cat, parsed, "Round-trip failed for {:?}", cat);
        }
    }

    #[test]
    fn test_review_category_parse_unknown() {
        assert_eq!(
            ReviewCategory::parse("unknown_category"),
            ReviewCategory::Other
        );
        assert_eq!(ReviewCategory::parse(""), ReviewCategory::Other);
    }

    #[test]
    fn test_change_category_serde_roundtrip() {
        let cat = ChangeCategory::Tests;
        let json = serde_json::to_string(&cat).unwrap();
        let parsed: ChangeCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(cat, parsed);
    }

    #[test]
    fn test_review_category_serde_roundtrip() {
        let cat = ReviewCategory::Security;
        let json = serde_json::to_string(&cat).unwrap();
        let parsed: ReviewCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(cat, parsed);
    }

    #[test]
    fn test_diff_analysis_serde() {
        let analysis = DiffAnalysis {
            id: 1,
            attempt_id: 42,
            pr_url: "https://github.com/org/repo/pull/1".to_string(),
            github_repo: "org/repo".to_string(),
            pr_number: 1,
            files_changed: vec!["src/main.rs".to_string()],
            file_types: {
                let mut m = std::collections::HashMap::new();
                m.insert("rs".to_string(), 1);
                m
            },
            change_categories: vec![ChangeCategory::Modification],
            diff_summary: "1 file changed".to_string(),
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&analysis).unwrap();
        let parsed: DiffAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.attempt_id, 42);
        assert_eq!(parsed.files_changed.len(), 1);
        assert_eq!(parsed.change_categories[0], ChangeCategory::Modification);
    }

    #[test]
    fn test_fix_quality_score_serde() {
        let score = FixQualityScore {
            score: 0.87,
            merge_speed_component: 0.9,
            review_cycles_component: 0.8,
            approval_component: 1.0,
        };
        let json = serde_json::to_string(&score).unwrap();
        let parsed: FixQualityScore = serde_json::from_str(&json).unwrap();
        assert!((parsed.score - 0.87).abs() < f64::EPSILON);
    }

    #[test]
    fn test_strategy_fingerprint_serde() {
        let fp = StrategyFingerprint {
            id: 1,
            attempt_id: 10,
            files_explored: vec!["src/a.rs".to_string()],
            tests_run: 3,
            tools_used: {
                let mut m = std::collections::HashMap::new();
                m.insert("Read".to_string(), 5);
                m
            },
            fix_approach: "tdd".to_string(),
            strategy_summary: "test".to_string(),
            fix_quality_score: Some(0.9),
            created_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&fp).unwrap();
        let parsed: StrategyFingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fix_approach, "tdd");
        assert_eq!(*parsed.tools_used.get("Read").unwrap(), 5);
    }

    #[test]
    fn test_issue_cluster_serde() {
        let now = chrono::Utc::now();
        let cluster = IssueCluster {
            id: 1,
            cluster_key: "cluster_abc".to_string(),
            source: "sentry".to_string(),
            issue_ids: vec!["a".to_string(), "b".to_string()],
            window_start: now,
            window_end: now + chrono::Duration::minutes(15),
            resolved_by_issue_id: Some("a".to_string()),
            resolved_by_attempt_id: Some(42),
            status: "resolved".to_string(),
            created_at: now,
        };
        let json = serde_json::to_string(&cluster).unwrap();
        let parsed: IssueCluster = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cluster_key, "cluster_abc");
        assert_eq!(parsed.issue_ids.len(), 2);
        assert_eq!(parsed.resolved_by_issue_id, Some("a".to_string()));
    }

    #[test]
    fn test_extracted_learnings_default() {
        let learnings = ExtractedLearnings {
            root_cause: None,
            files_modified: vec![],
            strategy_used: None,
            tests_added: false,
            key_decisions: vec![],
        };
        let json = serde_json::to_string(&learnings).unwrap();
        let parsed: ExtractedLearnings = serde_json::from_str(&json).unwrap();
        assert!(parsed.root_cause.is_none());
        assert!(parsed.files_modified.is_empty());
    }

    #[test]
    fn test_promoted_instruction_serde() {
        let instruction = PromotedInstruction {
            id: 1,
            repo: "org/repo".to_string(),
            source_type: "qa_promotion".to_string(),
            instruction_text: "Always add tests".to_string(),
            occurrence_count: 5,
            confidence: 0.85,
            is_active: true,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let json = serde_json::to_string(&instruction).unwrap();
        let parsed: PromotedInstruction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.instruction_text, "Always add tests");
        assert!(parsed.is_active);
    }

    // ── Edge case tests ──

    #[test]
    fn test_validate_issue_id_exactly_max_length() {
        let id = "a".repeat(MAX_ISSUE_ID_LENGTH);
        assert!(validate_issue_id(&id).is_ok());
    }

    #[test]
    fn test_validate_issue_id_one_over_max() {
        let id = "a".repeat(MAX_ISSUE_ID_LENGTH + 1);
        assert!(validate_issue_id(&id).is_err());
    }

    #[test]
    fn test_validate_issue_id_embedded_null_byte() {
        assert!(validate_issue_id("abc\0def").is_err());
    }

    #[test]
    fn test_validate_issue_id_only_dots() {
        assert!(validate_issue_id("..").is_err());
    }

    #[test]
    fn test_validate_issue_id_contains_backslash() {
        assert!(validate_issue_id("a\\b").is_err());
    }

    #[test]
    fn test_validate_issue_id_single_char() {
        assert!(validate_issue_id("x").is_ok());
    }

    #[test]
    fn test_validate_issue_id_unicode_chars() {
        assert!(validate_issue_id("日本語").is_ok());
    }

    #[test]
    fn test_validate_issue_id_whitespace() {
        assert!(validate_issue_id("abc def").is_ok());
    }

    #[test]
    fn test_validate_issue_id_double_dot_in_middle() {
        assert!(validate_issue_id("foo..bar").is_err());
    }

    #[test]
    fn test_fix_attempt_is_bug_sentry_source() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "123".into(),
            short_id: "S-123".into(),
            source: "sentry".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_bug_with_bug_label() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "123".into(),
            short_id: "P-123".into(),
            source: "linear".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec!["bug".into()],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_bug_case_insensitive() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "1".into(),
            short_id: "T-1".into(),
            source: "linear".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec!["BUG".into()],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_bug_label_substring() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "1".into(),
            short_id: "T-1".into(),
            source: "linear".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec!["hotfix-urgent".into()],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_not_bug_feature_label() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "1".into(),
            short_id: "T-1".into(),
            source: "linear".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec!["feature".into(), "enhancement".into()],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(!attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_bug_empty_labels() {
        let attempt = FixAttempt {
            id: 1,
            issue_id: "1".into(),
            short_id: "T-1".into(),
            source: "linear".into(),
            attempted_at: Utc::now(),
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            status: FixAttemptStatus::Pending,
            error_message: None,
            merged_at: None,
            resolved_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };
        assert!(!attempt.is_bug());
    }

    #[test]
    fn test_fix_attempt_is_bug_all_bug_labels() {
        for label in [
            "bug",
            "defect",
            "error",
            "fix",
            "hotfix",
            "regression",
            "issue",
            "problem",
            "incident",
            "crash",
            "broken",
        ] {
            let attempt = FixAttempt {
                id: 1,
                issue_id: "1".into(),
                short_id: "T-1".into(),
                source: "linear".into(),
                attempted_at: Utc::now(),
                pr_url: None,
                github_repo: None,
                github_pr_number: None,
                status: FixAttemptStatus::Pending,
                error_message: None,
                merged_at: None,
                resolved_at: None,
                retry_count: 0,
                last_retry_at: None,
                issue_labels: vec![label.into()],
                parent_attempt_id: None,
                cascade_repo: None,
            };
            assert!(
                attempt.is_bug(),
                "Label '{}' should be detected as bug",
                label
            );
        }
    }

    #[test]
    fn test_claude_execution_complete() {
        let mut exec = ClaudeExecution::new();
        assert!(exec.completed_at.is_none());
        assert!(exec.duration_secs.is_none());
        exec.complete(Some(0), false);
        assert!(exec.completed_at.is_some());
        assert!(exec.duration_secs.is_some());
        assert_eq!(exec.exit_code, Some(0));
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_complete_timeout() {
        let mut exec = ClaudeExecution::new();
        exec.complete(None, true);
        assert!(exec.timed_out);
        assert!(exec.exit_code.is_none());
        assert!(exec.completed_at.is_some());
    }

    #[test]
    fn test_claude_execution_complete_nonzero_exit() {
        let mut exec = ClaudeExecution::new();
        exec.complete(Some(1), false);
        assert_eq!(exec.exit_code, Some(1));
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_with_attempt_id() {
        let exec = ClaudeExecution::new().with_attempt_id(42);
        assert_eq!(exec.attempt_id, Some(42));
    }

    #[test]
    fn test_claude_execution_default() {
        let exec = ClaudeExecution::default();
        assert_eq!(exec.id, 0);
        assert!(exec.attempt_id.is_none());
        assert!(!exec.timed_out);
    }

    #[test]
    fn test_claude_execution_duration_non_negative() {
        let mut exec = ClaudeExecution::new();
        exec.complete(Some(0), false);
        assert!(exec.duration_secs.unwrap() >= 0.0);
    }

    #[test]
    fn test_fix_attempt_status_display_roundtrip() {
        for status in [
            FixAttemptStatus::Pending,
            FixAttemptStatus::Success,
            FixAttemptStatus::Failed,
            FixAttemptStatus::Merged,
            FixAttemptStatus::Closed,
            FixAttemptStatus::CannotFix,
        ] {
            let parsed: FixAttemptStatus = status.to_string().parse().unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_fix_attempt_status_serde_roundtrip() {
        for status in [
            FixAttemptStatus::Pending,
            FixAttemptStatus::Success,
            FixAttemptStatus::Failed,
            FixAttemptStatus::Merged,
            FixAttemptStatus::Closed,
            FixAttemptStatus::CannotFix,
        ] {
            let json = serde_json::to_string(&status).unwrap();
            let parsed: FixAttemptStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_regression_watch_status_display_roundtrip() {
        for status in [
            RegressionWatchStatus::AwaitingRelease,
            RegressionWatchStatus::Monitoring,
            RegressionWatchStatus::Resolved,
            RegressionWatchStatus::Regressed,
        ] {
            let parsed: RegressionWatchStatus = status.to_string().parse().unwrap();
            assert_eq!(status, parsed);
        }
    }

    #[test]
    fn test_regression_watch_status_parse_invalid() {
        assert!("unknown".parse::<RegressionWatchStatus>().is_err());
        assert!("".parse::<RegressionWatchStatus>().is_err());
    }

    #[test]
    fn test_regression_watch_status_default_value() {
        assert_eq!(
            RegressionWatchStatus::default(),
            RegressionWatchStatus::AwaitingRelease
        );
    }

    #[test]
    fn test_issue_type_source_name() {
        assert_eq!(IssueType::SentryIssue.source_name(), "sentry");
        assert_eq!(IssueType::LinearBug.source_name(), "linear");
    }

    #[test]
    fn test_issue_type_display_parse_roundtrip() {
        for issue_type in [IssueType::SentryIssue, IssueType::LinearBug] {
            let parsed: IssueType = issue_type.to_string().parse().unwrap();
            assert_eq!(issue_type, parsed);
        }
    }

    #[test]
    fn test_issue_type_parse_invalid() {
        assert!("unknown".parse::<IssueType>().is_err());
        assert!("".parse::<IssueType>().is_err());
    }

    #[test]
    fn test_change_category_display_all() {
        assert_eq!(ChangeCategory::Tests.to_string(), "tests");
        assert_eq!(ChangeCategory::Docs.to_string(), "docs");
        assert_eq!(ChangeCategory::Config.to_string(), "config");
        assert_eq!(ChangeCategory::Dependencies.to_string(), "dependencies");
        assert_eq!(ChangeCategory::Migrations.to_string(), "migrations");
        assert_eq!(ChangeCategory::NewCode.to_string(), "new_code");
        assert_eq!(ChangeCategory::Modification.to_string(), "modification");
    }

    #[test]
    fn test_change_category_serde_all_variants() {
        for cat in [
            ChangeCategory::Tests,
            ChangeCategory::Docs,
            ChangeCategory::Config,
            ChangeCategory::Dependencies,
            ChangeCategory::Migrations,
            ChangeCategory::NewCode,
            ChangeCategory::Modification,
        ] {
            let json = serde_json::to_string(&cat).unwrap();
            let parsed: ChangeCategory = serde_json::from_str(&json).unwrap();
            assert_eq!(cat, parsed);
        }
    }

    #[test]
    fn test_review_category_display_all() {
        assert_eq!(ReviewCategory::MissingTests.to_string(), "missing_tests");
        assert_eq!(ReviewCategory::StyleIssue.to_string(), "style_issue");
        assert_eq!(ReviewCategory::WrongApproach.to_string(), "wrong_approach");
        assert_eq!(ReviewCategory::Incomplete.to_string(), "incomplete");
        assert_eq!(ReviewCategory::Security.to_string(), "security");
        assert_eq!(ReviewCategory::Performance.to_string(), "performance");
        assert_eq!(ReviewCategory::Documentation.to_string(), "documentation");
        assert_eq!(ReviewCategory::Other.to_string(), "other");
    }

    #[test]
    fn test_review_category_parse_all_variants() {
        for cat in [
            ReviewCategory::MissingTests,
            ReviewCategory::StyleIssue,
            ReviewCategory::WrongApproach,
            ReviewCategory::Incomplete,
            ReviewCategory::Security,
            ReviewCategory::Performance,
            ReviewCategory::Documentation,
            ReviewCategory::Other,
        ] {
            let parsed = ReviewCategory::parse(&cat.to_string());
            assert_eq!(cat, parsed);
        }
    }

    #[test]
    fn test_review_category_parse_unknown_returns_other() {
        assert_eq!(ReviewCategory::parse("unknown"), ReviewCategory::Other);
        assert_eq!(ReviewCategory::parse(""), ReviewCategory::Other);
        assert_eq!(ReviewCategory::parse("foobar"), ReviewCategory::Other);
    }

    #[test]
    fn test_activity_log_entry_builder_chain() {
        let entry = ActivityLogEntry::new("test_type", "test message")
            .with_source("linear".to_string())
            .with_issue("issue-1".to_string(), "PROJ-1".to_string())
            .with_metadata(serde_json::json!({"key": "value"}));
        assert_eq!(entry.activity_type, "test_type");
        assert_eq!(entry.source, Some("linear".to_string()));
        assert_eq!(entry.issue_id, Some("issue-1".to_string()));
        assert!(entry.metadata.is_some());
    }

    #[test]
    fn test_activity_log_entry_minimal() {
        let entry = ActivityLogEntry::new("type", "msg");
        assert_eq!(entry.id, 0);
        assert!(entry.source.is_none());
        assert!(entry.issue_id.is_none());
        assert!(entry.metadata.is_none());
    }

    #[test]
    fn test_pr_record_new() {
        let pr = PrRecord::new("https://github.com/org/repo/pull/1", "org/repo", 1);
        assert_eq!(pr.status, "open");
        assert_eq!(pr.approvals_count, 0);
        assert_eq!(pr.review_cycles, 0);
        assert!(pr.merged_at.is_none());
    }

    #[test]
    fn test_pr_record_for_issue() {
        let pr = PrRecord::for_issue("url", "org/repo", 1, "linear", "issue-1");
        assert_eq!(pr.issue_source, Some("linear".to_string()));
        assert_eq!(pr.issue_id, Some("issue-1".to_string()));
    }

    #[test]
    fn test_error_pattern_new() {
        let pattern = ErrorPattern::new("abc123");
        assert_eq!(pattern.pattern_hash, "abc123");
        assert_eq!(pattern.occurrence_count, 1);
        assert!(pattern.error_type.is_none());
    }

    #[test]
    fn test_processing_metric_builder() {
        let metric = ProcessingMetric::new("queue_depth", 42.0)
            .with_source("linear")
            .with_tags(serde_json::json!({"env": "prod"}));
        assert_eq!(metric.metric_name, "queue_depth");
        assert_eq!(metric.metric_value, 42.0);
        assert!(metric.tags.is_some());
    }

    #[test]
    fn test_processing_metric_special_values() {
        let nan = ProcessingMetric::new("test", f64::NAN);
        assert!(nan.metric_value.is_nan());
        let inf = ProcessingMetric::new("test", f64::INFINITY);
        assert!(inf.metric_value.is_infinite());
    }

    #[test]
    fn test_similar_issue_new() {
        let si = SimilarIssue::new("a", "b", 0.95);
        assert_eq!(si.source_issue_id, "a");
        assert_eq!(si.similar_issue_id, "b");
        assert_eq!(si.id, 0);
    }

    #[test]
    fn test_issue_embedding_empty_vector() {
        let e = IssueEmbedding::new("s", "i", vec![]);
        assert!(e.embedding.as_ref().unwrap().is_empty());
    }

    #[test]
    fn test_prompt_experiment_new() {
        let exp = PromptExperiment::new("exp", "control", "tpl", "hash");
        assert!(exp.active);
        assert_eq!(exp.success_count, 0);
        assert_eq!(exp.failure_count, 0);
    }

    #[test]
    fn test_regression_watch_new_defaults() {
        let w = RegressionWatch::new(IssueType::SentryIssue, "i-1", 42);
        assert_eq!(w.status, RegressionWatchStatus::AwaitingRelease);
        assert!(w.pr_merged_at.is_none());
    }

    #[test]
    fn test_release_tracking_new_fields() {
        let rt = ReleaseTracking::new(1, "v1.0", "abc");
        assert!(rt.released_at.is_some());
    }

    #[test]
    fn test_regression_check_new_no_regression() {
        let c = RegressionCheck::new(1, false);
        assert!(!c.issue_still_exists);
        assert!(c.checked_at.is_some());
    }

    #[test]
    fn test_regression_check_regression_detected() {
        let c = RegressionCheck::new(1, true);
        assert!(c.issue_still_exists);
    }

    #[test]
    fn test_blocking_question_serde_full() {
        let q = BlockingQuestion {
            question: "API?".into(),
            context: Some("ctx".into()),
            options: vec!["REST".into(), "GraphQL".into()],
            why: Some("reason".into()),
        };
        let json = serde_json::to_string(&q).unwrap();
        let parsed: BlockingQuestion = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.options.len(), 2);
    }

    #[test]
    fn test_blocking_question_serde_minimal() {
        let q = BlockingQuestion {
            question: "Yes?".into(),
            context: None,
            options: vec![],
            why: None,
        };
        let json = serde_json::to_string(&q).unwrap();
        assert!(!json.contains("context"));
        assert!(!json.contains("why"));
    }

    #[test]
    fn test_match_result_not_matched_uses_normal_priority() {
        let r = MatchResult::not_matched("no match");
        assert!(!r.matches);
        assert_eq!(r.priority, MatchPriority::Normal);
    }

    #[test]
    fn test_issue_metadata_overwrite() {
        let mut issue = Issue::new("1", "T-1", "Title", "url", "src");
        issue.set_metadata("key", "first");
        issue.set_metadata("key", "second");
        assert_eq!(
            issue.get_metadata::<String>("key"),
            Some("second".to_string())
        );
    }

    #[test]
    fn test_pr_analytics_default() {
        let a = PrAnalytics::default();
        assert_eq!(a.total, 0);
        assert!(a.merge_rate.is_none());
        assert!(a.by_repo.is_empty());
    }

    #[test]
    fn test_analytics_summary_default() {
        let s = AnalyticsSummary::default();
        assert_eq!(s.success_rate, 0.0);
        assert!(s.most_common_error.is_none());
    }

    #[test]
    fn test_fix_quality_score_serde_roundtrip() {
        let score = FixQualityScore {
            score: 0.85,
            merge_speed_component: 0.9,
            review_cycles_component: 0.8,
            approval_component: 1.0,
        };
        let json = serde_json::to_string(&score).unwrap();
        let parsed: FixQualityScore = serde_json::from_str(&json).unwrap();
        assert!((parsed.score - 0.85).abs() < 0.001);
    }

    #[test]
    fn test_issue_cluster_serde_roundtrip() {
        let c = IssueCluster {
            id: 1,
            cluster_key: "k".into(),
            source: "sentry".into(),
            issue_ids: vec!["a".into(), "b".into()],
            window_start: Utc::now(),
            window_end: Utc::now(),
            resolved_by_issue_id: None,
            resolved_by_attempt_id: None,
            status: "active".into(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: IssueCluster = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.issue_ids.len(), 2);
    }

    #[test]
    fn test_strategy_fingerprint_serde_roundtrip() {
        let mut tools = HashMap::new();
        tools.insert("Read".into(), 5i64);
        let fp = StrategyFingerprint {
            id: 1,
            attempt_id: 42,
            files_explored: vec!["src/main.rs".into()],
            tests_run: 2,
            tools_used: tools,
            fix_approach: "tdd".into(),
            strategy_summary: "summary".into(),
            fix_quality_score: Some(0.9),
            created_at: Utc::now(),
        };
        let json = serde_json::to_string(&fp).unwrap();
        let parsed: StrategyFingerprint = serde_json::from_str(&json).unwrap();
        assert_eq!(*parsed.tools_used.get("Read").unwrap(), 5);
    }

    #[test]
    fn test_repo_knowledge_serde() {
        let k = RepoKnowledge {
            id: 1,
            repo: "org/repo".into(),
            knowledge_key: "test_pattern".into(),
            knowledge_value: "cargo test".into(),
            source_type: "diff".into(),
            confidence: 0.9,
            occurrence_count: 5,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&k).unwrap();
        let parsed: RepoKnowledge = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.knowledge_key, "test_pattern");
    }

    #[test]
    fn test_review_pattern_serde() {
        let p = ReviewPattern {
            id: 1,
            github_repo: "org/repo".into(),
            category: ReviewCategory::MissingTests,
            pattern_text: "Add tests".into(),
            example_comments: vec!["please add tests".into()],
            occurrence_count: 3,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let json = serde_json::to_string(&p).unwrap();
        let parsed: ReviewPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.category, ReviewCategory::MissingTests);
    }

    #[test]
    fn test_qa_match_serde() {
        let entry = QaKnowledgeEntry {
            id: 1,
            source: "l".into(),
            repo: None,
            issue_id: "i".into(),
            short_id: "I".into(),
            question_text: "Q".into(),
            question_norm: "q".into(),
            question_embedding: None,
            answer_text: "A".into(),
            answer_norm: "a".into(),
            answer_embedding: None,
            channel: "ch".into(),
            responder: None,
            correlation_id: "c".into(),
            asked_at: Utc::now(),
            answered_at: Utc::now(),
            success_count: 1,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };
        let m = QaMatch {
            entry,
            semantic_similarity: 0.95,
            historical_success_rate: 1.0,
            final_score: 0.97,
        };
        let json = serde_json::to_string(&m).unwrap();
        let parsed: QaMatch = serde_json::from_str(&json).unwrap();
        assert!((parsed.final_score - 0.97).abs() < 0.001);
    }
}

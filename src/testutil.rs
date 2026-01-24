//! Test utilities and builders for creating test fixtures.
//!
//! This module provides builder patterns and helper functions for creating
//! test instances of core types. These utilities reduce boilerplate in tests
//! and ensure consistent test data across the codebase.
//!
//! # Example
//!
//! ```rust
//! use claudear::testutil::{IssueBuilder, FixAttemptBuilder};
//!
//! // Create a test issue with custom fields
//! let issue = IssueBuilder::new()
//!     .id("TEST-123")
//!     .title("Fix authentication bug")
//!     .priority(claudear::IssuePriority::High)
//!     .build();
//!
//! // Create a test fix attempt
//! let attempt = FixAttemptBuilder::new()
//!     .issue_id("TEST-123")
//!     .status(claudear::FixAttemptStatus::Success)
//!     .pr_url("https://github.com/org/repo/pull/42")
//!     .build();
//! ```

use crate::types::{
    ActivityLogEntry, AnalyticsSummary, ClaudeExecution, ClaudeResult, ErrorPattern, FixAttempt,
    FixAttemptStats, FixAttemptStatus, Issue, IssuePriority, IssueStatus, MatchPriority,
    MatchResult, ProcessingMetric, PromptExperiment, SourceStats,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;

/// Builder for creating test [`Issue`] instances.
///
/// Provides sensible defaults for all fields while allowing customization
/// of specific fields as needed for individual tests.
#[derive(Debug, Clone)]
pub struct IssueBuilder {
    id: String,
    short_id: String,
    title: String,
    description: Option<String>,
    url: String,
    source: String,
    priority: IssuePriority,
    status: IssueStatus,
    metadata: HashMap<String, serde_json::Value>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
}

impl Default for IssueBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IssueBuilder {
    /// Create a new builder with default test values.
    pub fn new() -> Self {
        Self {
            id: "test-id-001".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Test Issue Title".to_string(),
            description: None,
            url: "https://example.com/issues/test-1".to_string(),
            source: "test".to_string(),
            priority: IssuePriority::None,
            status: IssueStatus::Open,
            metadata: HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    /// Create a builder pre-configured for Linear issues.
    pub fn linear() -> Self {
        Self::new()
            .source("linear")
            .short_id("LIN-123")
            .url("https://linear.app/team/issue/LIN-123")
    }

    /// Create a builder pre-configured for Sentry issues.
    pub fn sentry() -> Self {
        Self::new()
            .source("sentry")
            .short_id("SENTRY-456")
            .url("https://sentry.io/issues/456")
    }

    /// Set the issue ID.
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.id = id.into();
        self
    }

    /// Set the short (human-readable) ID.
    pub fn short_id(mut self, short_id: impl Into<String>) -> Self {
        self.short_id = short_id.into();
        self
    }

    /// Set the issue title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Set the issue description.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set the issue URL.
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = url.into();
        self
    }

    /// Set the source service name.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Set the priority level.
    pub fn priority(mut self, priority: IssuePriority) -> Self {
        self.priority = priority;
        self
    }

    /// Set the issue status.
    pub fn status(mut self, status: IssueStatus) -> Self {
        self.status = status;
        self
    }

    /// Add a metadata entry.
    pub fn with_metadata(mut self, key: impl Into<String>, value: impl serde::Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(value) {
            self.metadata.insert(key.into(), v);
        }
        self
    }

    /// Set the created_at timestamp.
    pub fn created_at(mut self, created_at: DateTime<Utc>) -> Self {
        self.created_at = Some(created_at);
        self
    }

    /// Set the updated_at timestamp.
    pub fn updated_at(mut self, updated_at: DateTime<Utc>) -> Self {
        self.updated_at = Some(updated_at);
        self
    }

    /// Build the [`Issue`] instance.
    pub fn build(self) -> Issue {
        Issue {
            id: self.id,
            short_id: self.short_id,
            title: self.title,
            description: self.description,
            url: self.url,
            source: self.source,
            priority: self.priority,
            status: self.status,
            metadata: self.metadata,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Builder for creating test [`FixAttempt`] instances.
#[derive(Debug, Clone)]
pub struct FixAttemptBuilder {
    id: i64,
    issue_id: String,
    short_id: String,
    source: String,
    attempted_at: DateTime<Utc>,
    pr_url: Option<String>,
    github_repo: Option<String>,
    github_pr_number: Option<i64>,
    status: FixAttemptStatus,
    error_message: Option<String>,
    merged_at: Option<DateTime<Utc>>,
    resolved_at: Option<DateTime<Utc>>,
    retry_count: u32,
    last_retry_at: Option<DateTime<Utc>>,
}

impl Default for FixAttemptBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FixAttemptBuilder {
    /// Create a new builder with default test values.
    pub fn new() -> Self {
        Self {
            id: 1,
            issue_id: "test-id-001".to_string(),
            short_id: "TEST-1".to_string(),
            source: "test".to_string(),
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
        }
    }

    /// Create a builder configured as a successful attempt with a PR.
    pub fn successful() -> Self {
        Self::new()
            .status(FixAttemptStatus::Success)
            .pr_url("https://github.com/org/repo/pull/1")
            .github_repo("org/repo")
            .github_pr_number(1)
    }

    /// Create a builder configured as a failed attempt.
    pub fn failed() -> Self {
        Self::new()
            .status(FixAttemptStatus::Failed)
            .error_message("Build failed")
    }

    /// Create a builder configured as a merged attempt.
    pub fn merged() -> Self {
        Self::successful()
            .status(FixAttemptStatus::Merged)
            .merged_at(Utc::now())
            .resolved_at(Utc::now())
    }

    /// Set the database ID.
    pub fn id(mut self, id: i64) -> Self {
        self.id = id;
        self
    }

    /// Set the issue ID.
    pub fn issue_id(mut self, issue_id: impl Into<String>) -> Self {
        self.issue_id = issue_id.into();
        self
    }

    /// Set the short (human-readable) ID.
    pub fn short_id(mut self, short_id: impl Into<String>) -> Self {
        self.short_id = short_id.into();
        self
    }

    /// Set the source service name.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    /// Set the attempted_at timestamp.
    pub fn attempted_at(mut self, attempted_at: DateTime<Utc>) -> Self {
        self.attempted_at = attempted_at;
        self
    }

    /// Set the PR URL.
    pub fn pr_url(mut self, pr_url: impl Into<String>) -> Self {
        self.pr_url = Some(pr_url.into());
        self
    }

    /// Set the GitHub repository (owner/repo format).
    pub fn github_repo(mut self, repo: impl Into<String>) -> Self {
        self.github_repo = Some(repo.into());
        self
    }

    /// Set the GitHub PR number.
    pub fn github_pr_number(mut self, pr_number: i64) -> Self {
        self.github_pr_number = Some(pr_number);
        self
    }

    /// Set the fix attempt status.
    pub fn status(mut self, status: FixAttemptStatus) -> Self {
        self.status = status;
        self
    }

    /// Set the error message.
    pub fn error_message(mut self, error_message: impl Into<String>) -> Self {
        self.error_message = Some(error_message.into());
        self
    }

    /// Set the merged_at timestamp.
    pub fn merged_at(mut self, merged_at: DateTime<Utc>) -> Self {
        self.merged_at = Some(merged_at);
        self
    }

    /// Set the resolved_at timestamp.
    pub fn resolved_at(mut self, resolved_at: DateTime<Utc>) -> Self {
        self.resolved_at = Some(resolved_at);
        self
    }

    /// Set the retry count.
    pub fn retry_count(mut self, retry_count: u32) -> Self {
        self.retry_count = retry_count;
        self
    }

    /// Set the last_retry_at timestamp.
    pub fn last_retry_at(mut self, last_retry_at: DateTime<Utc>) -> Self {
        self.last_retry_at = Some(last_retry_at);
        self
    }

    /// Build the [`FixAttempt`] instance.
    pub fn build(self) -> FixAttempt {
        FixAttempt {
            id: self.id,
            issue_id: self.issue_id,
            short_id: self.short_id,
            source: self.source,
            attempted_at: self.attempted_at,
            pr_url: self.pr_url,
            github_repo: self.github_repo,
            github_pr_number: self.github_pr_number,
            status: self.status,
            error_message: self.error_message,
            merged_at: self.merged_at,
            resolved_at: self.resolved_at,
            retry_count: self.retry_count,
            last_retry_at: self.last_retry_at,
        }
    }
}

/// Builder for creating test [`ClaudeResult`] instances.
#[derive(Debug, Clone)]
pub struct ClaudeResultBuilder {
    success: bool,
    output: String,
    pr_url: Option<String>,
    error: Option<String>,
}

impl Default for ClaudeResultBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeResultBuilder {
    /// Create a new builder with default (pending) values.
    pub fn new() -> Self {
        Self {
            success: false,
            output: String::new(),
            pr_url: None,
            error: None,
        }
    }

    /// Create a builder configured as a successful result.
    pub fn successful() -> Self {
        Self {
            success: true,
            output: "PR created successfully".to_string(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            error: None,
        }
    }

    /// Create a builder configured as a failed result.
    pub fn failed() -> Self {
        Self {
            success: false,
            output: String::new(),
            pr_url: None,
            error: Some("Build failed".to_string()),
        }
    }

    /// Set whether the result is successful.
    pub fn success(mut self, success: bool) -> Self {
        self.success = success;
        self
    }

    /// Set the output text.
    pub fn output(mut self, output: impl Into<String>) -> Self {
        self.output = output.into();
        self
    }

    /// Set the PR URL.
    pub fn pr_url(mut self, pr_url: impl Into<String>) -> Self {
        self.pr_url = Some(pr_url.into());
        self
    }

    /// Set the error message.
    pub fn error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Build the [`ClaudeResult`] instance.
    pub fn build(self) -> ClaudeResult {
        ClaudeResult {
            success: self.success,
            output: self.output,
            pr_url: self.pr_url,
            error: self.error,
        }
    }
}

/// Builder for creating test [`MatchResult`] instances.
#[derive(Debug, Clone)]
pub struct MatchResultBuilder {
    matches: bool,
    reason: String,
    priority: MatchPriority,
}

impl Default for MatchResultBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchResultBuilder {
    /// Create a new builder with default values (not matched).
    pub fn new() -> Self {
        Self {
            matches: false,
            reason: "No match".to_string(),
            priority: MatchPriority::Normal,
        }
    }

    /// Create a builder configured as a matching result.
    pub fn matched() -> Self {
        Self {
            matches: true,
            reason: "Matches criteria".to_string(),
            priority: MatchPriority::Normal,
        }
    }

    /// Set whether it matches.
    pub fn matches(mut self, matches: bool) -> Self {
        self.matches = matches;
        self
    }

    /// Set the reason.
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    /// Set the priority.
    pub fn priority(mut self, priority: MatchPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Build the [`MatchResult`] instance.
    pub fn build(self) -> MatchResult {
        MatchResult {
            matches: self.matches,
            reason: self.reason,
            priority: self.priority,
        }
    }
}

/// Builder for creating test [`ActivityLogEntry`] instances.
#[derive(Debug, Clone)]
pub struct ActivityLogEntryBuilder {
    id: i64,
    timestamp: DateTime<Utc>,
    activity_type: String,
    source: Option<String>,
    issue_id: Option<String>,
    short_id: Option<String>,
    message: String,
    metadata: Option<serde_json::Value>,
}

impl Default for ActivityLogEntryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ActivityLogEntryBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            id: 1,
            timestamp: Utc::now(),
            activity_type: "test_activity".to_string(),
            source: None,
            issue_id: None,
            short_id: None,
            message: "Test activity message".to_string(),
            metadata: None,
        }
    }

    /// Set the database ID.
    pub fn id(mut self, id: i64) -> Self {
        self.id = id;
        self
    }

    /// Set the timestamp.
    pub fn timestamp(mut self, timestamp: DateTime<Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Set the activity type.
    pub fn activity_type(mut self, activity_type: impl Into<String>) -> Self {
        self.activity_type = activity_type.into();
        self
    }

    /// Set the source.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// Set the issue ID.
    pub fn issue_id(mut self, issue_id: impl Into<String>) -> Self {
        self.issue_id = Some(issue_id.into());
        self
    }

    /// Set the short ID.
    pub fn short_id(mut self, short_id: impl Into<String>) -> Self {
        self.short_id = Some(short_id.into());
        self
    }

    /// Set the message.
    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// Set the metadata.
    pub fn metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Build the [`ActivityLogEntry`] instance.
    pub fn build(self) -> ActivityLogEntry {
        ActivityLogEntry {
            id: self.id,
            timestamp: self.timestamp,
            activity_type: self.activity_type,
            source: self.source,
            issue_id: self.issue_id,
            short_id: self.short_id,
            message: self.message,
            metadata: self.metadata,
        }
    }
}

/// Builder for creating test [`ClaudeExecution`] instances.
#[derive(Debug, Clone)]
pub struct ClaudeExecutionBuilder {
    inner: ClaudeExecution,
}

impl Default for ClaudeExecutionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeExecutionBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            inner: ClaudeExecution::new(),
        }
    }

    /// Create a builder configured as a completed execution.
    pub fn completed() -> Self {
        let mut builder = Self::new();
        builder.inner.completed_at = Some(Utc::now());
        builder.inner.duration_secs = Some(120.5);
        builder.inner.exit_code = Some(0);
        builder
    }

    /// Create a builder configured as a timed out execution.
    pub fn timed_out() -> Self {
        let mut builder = Self::new();
        builder.inner.timed_out = true;
        builder.inner.completed_at = Some(Utc::now());
        builder.inner.duration_secs = Some(21600.0); // 6 hours
        builder
    }

    /// Set the database ID.
    pub fn id(mut self, id: i64) -> Self {
        self.inner.id = id;
        self
    }

    /// Set the attempt ID.
    pub fn attempt_id(mut self, attempt_id: i64) -> Self {
        self.inner.attempt_id = Some(attempt_id);
        self
    }

    /// Set the exit code.
    pub fn exit_code(mut self, exit_code: i32) -> Self {
        self.inner.exit_code = Some(exit_code);
        self
    }

    /// Set the stdout preview.
    pub fn stdout_preview(mut self, stdout: impl Into<String>) -> Self {
        self.inner.stdout_preview = Some(stdout.into());
        self
    }

    /// Set the stderr preview.
    pub fn stderr_preview(mut self, stderr: impl Into<String>) -> Self {
        self.inner.stderr_preview = Some(stderr.into());
        self
    }

    /// Set the working directory.
    pub fn working_directory(mut self, dir: impl Into<String>) -> Self {
        self.inner.working_directory = Some(dir.into());
        self
    }

    /// Set the git branch.
    pub fn git_branch(mut self, branch: impl Into<String>) -> Self {
        self.inner.git_branch = Some(branch.into());
        self
    }

    /// Set the files changed count.
    pub fn files_changed(mut self, count: i32) -> Self {
        self.inner.files_changed = Some(count);
        self
    }

    /// Set the lines added count.
    pub fn lines_added(mut self, count: i32) -> Self {
        self.inner.lines_added = Some(count);
        self
    }

    /// Set the lines removed count.
    pub fn lines_removed(mut self, count: i32) -> Self {
        self.inner.lines_removed = Some(count);
        self
    }

    /// Build the [`ClaudeExecution`] instance.
    pub fn build(self) -> ClaudeExecution {
        self.inner
    }
}

/// Builder for creating test [`ErrorPattern`] instances.
#[derive(Debug, Clone)]
pub struct ErrorPatternBuilder {
    inner: ErrorPattern,
}

impl Default for ErrorPatternBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ErrorPatternBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            inner: ErrorPattern::new("test-pattern-hash"),
        }
    }

    /// Set the pattern hash.
    pub fn pattern_hash(mut self, hash: impl Into<String>) -> Self {
        self.inner.pattern_hash = hash.into();
        self
    }

    /// Set the error type.
    pub fn error_type(mut self, error_type: impl Into<String>) -> Self {
        self.inner.error_type = Some(error_type.into());
        self
    }

    /// Set the error message.
    pub fn error_message(mut self, message: impl Into<String>) -> Self {
        self.inner.error_message = Some(message.into());
        self
    }

    /// Set the occurrence count.
    pub fn occurrence_count(mut self, count: i32) -> Self {
        self.inner.occurrence_count = count;
        self
    }

    /// Set resolution hints.
    pub fn resolution_hints(mut self, hints: impl Into<String>) -> Self {
        self.inner.resolution_hints = Some(hints.into());
        self
    }

    /// Build the [`ErrorPattern`] instance.
    pub fn build(self) -> ErrorPattern {
        self.inner
    }
}

/// Builder for creating test [`ProcessingMetric`] instances.
#[derive(Debug, Clone)]
pub struct ProcessingMetricBuilder {
    inner: ProcessingMetric,
}

impl Default for ProcessingMetricBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessingMetricBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            inner: ProcessingMetric::new("test_metric", 0.0),
        }
    }

    /// Set the metric name.
    pub fn metric_name(mut self, name: impl Into<String>) -> Self {
        self.inner.metric_name = name.into();
        self
    }

    /// Set the metric value.
    pub fn metric_value(mut self, value: f64) -> Self {
        self.inner.metric_value = value;
        self
    }

    /// Set the source.
    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.inner.source = Some(source.into());
        self
    }

    /// Set the tags.
    pub fn tags(mut self, tags: serde_json::Value) -> Self {
        self.inner.tags = Some(tags);
        self
    }

    /// Build the [`ProcessingMetric`] instance.
    pub fn build(self) -> ProcessingMetric {
        self.inner
    }
}

/// Builder for creating test [`PromptExperiment`] instances.
#[derive(Debug, Clone)]
pub struct PromptExperimentBuilder {
    inner: PromptExperiment,
}

impl Default for PromptExperimentBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptExperimentBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self {
            inner: PromptExperiment::new(
                "test_experiment",
                "control",
                "Test prompt template",
                "test-hash",
            ),
        }
    }

    /// Set the experiment name.
    pub fn experiment_name(mut self, name: impl Into<String>) -> Self {
        self.inner.experiment_name = name.into();
        self
    }

    /// Set the variant.
    pub fn variant(mut self, variant: impl Into<String>) -> Self {
        self.inner.variant = variant.into();
        self
    }

    /// Set the prompt template.
    pub fn prompt_template(mut self, template: impl Into<String>) -> Self {
        self.inner.prompt_template = template.into();
        self
    }

    /// Set whether the experiment is active.
    pub fn active(mut self, active: bool) -> Self {
        self.inner.active = active;
        self
    }

    /// Set the success count.
    pub fn success_count(mut self, count: i32) -> Self {
        self.inner.success_count = count;
        self
    }

    /// Set the failure count.
    pub fn failure_count(mut self, count: i32) -> Self {
        self.inner.failure_count = count;
        self
    }

    /// Build the [`PromptExperiment`] instance.
    pub fn build(self) -> PromptExperiment {
        self.inner
    }
}

/// Builder for creating test [`FixAttemptStats`] instances.
#[derive(Debug, Clone, Default)]
pub struct FixAttemptStatsBuilder {
    inner: FixAttemptStats,
}

impl FixAttemptStatsBuilder {
    /// Create a new builder with default (zero) values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the total count.
    pub fn total(mut self, total: usize) -> Self {
        self.inner.total = total;
        self
    }

    /// Set the pending count.
    pub fn pending(mut self, pending: usize) -> Self {
        self.inner.pending = pending;
        self
    }

    /// Set the success count.
    pub fn success(mut self, success: usize) -> Self {
        self.inner.success = success;
        self
    }

    /// Set the failed count.
    pub fn failed(mut self, failed: usize) -> Self {
        self.inner.failed = failed;
        self
    }

    /// Set the merged count.
    pub fn merged(mut self, merged: usize) -> Self {
        self.inner.merged = merged;
        self
    }

    /// Set the closed count.
    pub fn closed(mut self, closed: usize) -> Self {
        self.inner.closed = closed;
        self
    }

    /// Set the cannot_fix count.
    pub fn cannot_fix(mut self, cannot_fix: usize) -> Self {
        self.inner.cannot_fix = cannot_fix;
        self
    }

    /// Add source-specific stats.
    pub fn with_source_stats(mut self, source: impl Into<String>, stats: SourceStats) -> Self {
        self.inner.by_source.insert(source.into(), stats);
        self
    }

    /// Build the [`FixAttemptStats`] instance.
    pub fn build(self) -> FixAttemptStats {
        self.inner
    }
}

/// Builder for creating test [`SourceStats`] instances.
#[derive(Debug, Clone, Default)]
pub struct SourceStatsBuilder {
    inner: SourceStats,
}

impl SourceStatsBuilder {
    /// Create a new builder with default (zero) values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the total count.
    pub fn total(mut self, total: usize) -> Self {
        self.inner.total = total;
        self
    }

    /// Set the success count.
    pub fn success(mut self, success: usize) -> Self {
        self.inner.success = success;
        self
    }

    /// Set the failed count.
    pub fn failed(mut self, failed: usize) -> Self {
        self.inner.failed = failed;
        self
    }

    /// Set the merged count.
    pub fn merged(mut self, merged: usize) -> Self {
        self.inner.merged = merged;
        self
    }

    /// Set the closed count.
    pub fn closed(mut self, closed: usize) -> Self {
        self.inner.closed = closed;
        self
    }

    /// Set the cannot_fix count.
    pub fn cannot_fix(mut self, cannot_fix: usize) -> Self {
        self.inner.cannot_fix = cannot_fix;
        self
    }

    /// Build the [`SourceStats`] instance.
    pub fn build(self) -> SourceStats {
        self.inner
    }
}

/// Builder for creating test [`AnalyticsSummary`] instances.
#[derive(Debug, Clone, Default)]
pub struct AnalyticsSummaryBuilder {
    inner: AnalyticsSummary,
}

impl AnalyticsSummaryBuilder {
    /// Create a new builder with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the success rate.
    pub fn success_rate(mut self, rate: f64) -> Self {
        self.inner.success_rate = rate;
        self
    }

    /// Set the total processed count.
    pub fn total_processed(mut self, count: i64) -> Self {
        self.inner.total_processed = count;
        self
    }

    /// Set the total successful count.
    pub fn total_successful(mut self, count: i64) -> Self {
        self.inner.total_successful = count;
        self
    }

    /// Set the total merged count.
    pub fn total_merged(mut self, count: i64) -> Self {
        self.inner.total_merged = count;
        self
    }

    /// Set the average processing time.
    pub fn avg_processing_time_secs(mut self, secs: f64) -> Self {
        self.inner.avg_processing_time_secs = Some(secs);
        self
    }

    /// Set the average time to merge.
    pub fn avg_time_to_merge_hours(mut self, hours: f64) -> Self {
        self.inner.avg_time_to_merge_hours = Some(hours);
        self
    }

    /// Set the most common error.
    pub fn most_common_error(mut self, error: impl Into<String>) -> Self {
        self.inner.most_common_error = Some(error.into());
        self
    }

    /// Add a source success rate.
    pub fn with_source_success_rate(mut self, source: impl Into<String>, rate: f64) -> Self {
        self.inner.success_rate_by_source.insert(source.into(), rate);
        self
    }

    /// Build the [`AnalyticsSummary`] instance.
    pub fn build(self) -> AnalyticsSummary {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_builder_defaults() {
        let issue = IssueBuilder::new().build();
        assert_eq!(issue.id, "test-id-001");
        assert_eq!(issue.short_id, "TEST-1");
        assert_eq!(issue.source, "test");
        assert_eq!(issue.priority, IssuePriority::None);
        assert_eq!(issue.status, IssueStatus::Open);
    }

    #[test]
    fn test_issue_builder_linear() {
        let issue = IssueBuilder::linear().build();
        assert_eq!(issue.source, "linear");
        assert!(issue.url.contains("linear.app"));
    }

    #[test]
    fn test_issue_builder_sentry() {
        let issue = IssueBuilder::sentry().build();
        assert_eq!(issue.source, "sentry");
        assert!(issue.url.contains("sentry.io"));
    }

    #[test]
    fn test_issue_builder_custom_fields() {
        let issue = IssueBuilder::new()
            .id("custom-id")
            .title("Custom Title")
            .description("Custom description")
            .priority(IssuePriority::Critical)
            .status(IssueStatus::InProgress)
            .with_metadata("key", "value")
            .build();

        assert_eq!(issue.id, "custom-id");
        assert_eq!(issue.title, "Custom Title");
        assert_eq!(issue.description.as_deref(), Some("Custom description"));
        assert_eq!(issue.priority, IssuePriority::Critical);
        assert_eq!(issue.status, IssueStatus::InProgress);
        assert!(issue.metadata.contains_key("key"));
    }

    #[test]
    fn test_fix_attempt_builder_defaults() {
        let attempt = FixAttemptBuilder::new().build();
        assert_eq!(attempt.status, FixAttemptStatus::Pending);
        assert!(attempt.pr_url.is_none());
        assert_eq!(attempt.retry_count, 0);
    }

    #[test]
    fn test_fix_attempt_builder_successful() {
        let attempt = FixAttemptBuilder::successful().build();
        assert_eq!(attempt.status, FixAttemptStatus::Success);
        assert!(attempt.pr_url.is_some());
        assert!(attempt.github_repo.is_some());
        assert!(attempt.github_pr_number.is_some());
    }

    #[test]
    fn test_fix_attempt_builder_failed() {
        let attempt = FixAttemptBuilder::failed().build();
        assert_eq!(attempt.status, FixAttemptStatus::Failed);
        assert!(attempt.error_message.is_some());
    }

    #[test]
    fn test_fix_attempt_builder_merged() {
        let attempt = FixAttemptBuilder::merged().build();
        assert_eq!(attempt.status, FixAttemptStatus::Merged);
        assert!(attempt.merged_at.is_some());
        assert!(attempt.resolved_at.is_some());
    }

    #[test]
    fn test_claude_result_builder_successful() {
        let result = ClaudeResultBuilder::successful().build();
        assert!(result.success);
        assert!(result.pr_url.is_some());
        assert!(result.error.is_none());
    }

    #[test]
    fn test_claude_result_builder_failed() {
        let result = ClaudeResultBuilder::failed().build();
        assert!(!result.success);
        assert!(result.pr_url.is_none());
        assert!(result.error.is_some());
    }

    #[test]
    fn test_match_result_builder() {
        let result = MatchResultBuilder::matched()
            .priority(MatchPriority::Urgent)
            .reason("High priority issue")
            .build();

        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
        assert_eq!(result.reason, "High priority issue");
    }

    #[test]
    fn test_activity_log_entry_builder() {
        let entry = ActivityLogEntryBuilder::new()
            .activity_type("issue_received")
            .source("linear")
            .issue_id("123")
            .short_id("LIN-123")
            .message("New issue received")
            .build();

        assert_eq!(entry.activity_type, "issue_received");
        assert_eq!(entry.source.as_deref(), Some("linear"));
        assert_eq!(entry.issue_id.as_deref(), Some("123"));
    }

    #[test]
    fn test_claude_execution_builder() {
        let execution = ClaudeExecutionBuilder::completed()
            .files_changed(5)
            .lines_added(100)
            .lines_removed(20)
            .build();

        assert!(execution.completed_at.is_some());
        assert_eq!(execution.files_changed, Some(5));
        assert_eq!(execution.lines_added, Some(100));
        assert_eq!(execution.lines_removed, Some(20));
    }

    #[test]
    fn test_claude_execution_builder_timed_out() {
        let execution = ClaudeExecutionBuilder::timed_out().build();
        assert!(execution.timed_out);
        assert!(execution.completed_at.is_some());
    }

    #[test]
    fn test_error_pattern_builder() {
        let pattern = ErrorPatternBuilder::new()
            .error_type("build_failure")
            .error_message("Compilation error")
            .occurrence_count(5)
            .build();

        assert_eq!(pattern.error_type.as_deref(), Some("build_failure"));
        assert_eq!(pattern.occurrence_count, 5);
    }

    #[test]
    fn test_processing_metric_builder() {
        let metric = ProcessingMetricBuilder::new()
            .metric_name("queue_depth")
            .metric_value(42.0)
            .source("linear")
            .build();

        assert_eq!(metric.metric_name, "queue_depth");
        assert_eq!(metric.metric_value, 42.0);
        assert_eq!(metric.source.as_deref(), Some("linear"));
    }

    #[test]
    fn test_prompt_experiment_builder() {
        let experiment = PromptExperimentBuilder::new()
            .experiment_name("prompt_v2")
            .variant("variant_a")
            .success_count(10)
            .failure_count(2)
            .build();

        assert_eq!(experiment.experiment_name, "prompt_v2");
        assert_eq!(experiment.variant, "variant_a");
        assert_eq!(experiment.success_count, 10);
        assert_eq!(experiment.failure_count, 2);
    }

    #[test]
    fn test_fix_attempt_stats_builder() {
        let source_stats = SourceStatsBuilder::new()
            .total(50)
            .success(40)
            .failed(10)
            .build();

        let stats = FixAttemptStatsBuilder::new()
            .total(100)
            .success(80)
            .failed(20)
            .with_source_stats("linear", source_stats)
            .build();

        assert_eq!(stats.total, 100);
        assert_eq!(stats.success, 80);
        assert!(stats.by_source.contains_key("linear"));
    }

    #[test]
    fn test_analytics_summary_builder() {
        let summary = AnalyticsSummaryBuilder::new()
            .success_rate(0.85)
            .total_processed(100)
            .total_successful(85)
            .total_merged(70)
            .avg_processing_time_secs(120.5)
            .with_source_success_rate("linear", 0.90)
            .build();

        assert_eq!(summary.success_rate, 0.85);
        assert_eq!(summary.total_processed, 100);
        assert!(summary.success_rate_by_source.contains_key("linear"));
    }

    #[test]
    fn test_builder_default_trait() {
        // Verify Default trait works for all builders
        let _issue = IssueBuilder::default().build();
        let _attempt = FixAttemptBuilder::default().build();
        let _result = ClaudeResultBuilder::default().build();
        let _match_result = MatchResultBuilder::default().build();
        let _activity = ActivityLogEntryBuilder::default().build();
        let _execution = ClaudeExecutionBuilder::default().build();
        let _pattern = ErrorPatternBuilder::default().build();
        let _metric = ProcessingMetricBuilder::default().build();
        let _experiment = PromptExperimentBuilder::default().build();
        let _stats = FixAttemptStatsBuilder::default().build();
        let _source_stats = SourceStatsBuilder::default().build();
        let _summary = AnalyticsSummaryBuilder::default().build();
    }
}

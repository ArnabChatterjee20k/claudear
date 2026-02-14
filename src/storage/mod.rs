//! Storage implementations for tracking fix attempts, embeddings, and analytics.

pub mod analytics;
mod sqlite;
pub mod vectorlite;

pub use analytics::{
    classify_error, compute_error_hash, AnalyticsService, TimePeriod, TrendAnalysis, TrendDirection,
};
pub use sqlite::{
    ConfidenceBreakdown, DiagnosticCounts, IndexStats, InferenceHistoryEntry, InferenceStats,
    SqliteTracker, StoredDependency, StoredIndexedRepo, StoredRepository, UserRow,
};
pub use vectorlite::{is_vectorlite_available, try_load_vectorlite, VectorStoreConfig};

use crate::error::Result;
use crate::feedback::FixOutcome;
use crate::types::{
    ActivityLogEntry, AnalyticsSummary, ClaudeExecution, ErrorPattern, FixAttempt, FixAttemptStats,
    FixAttemptStatus, PrAnalytics, PrReviewRecord, ProcessingMetric, PromptExperiment,
    RegressionCheck, RegressionWatch, RegressionWatchStatus,
};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// Trait for tracking fix attempts.
pub trait FixAttemptTracker: Send + Sync {
    /// Check if an issue has already been attempted.
    fn has_attempted(&self, source: &str, issue_id: &str) -> bool;

    /// Get all attempted issue IDs for a source.
    fn get_attempted_issue_ids(&self, source: &str) -> HashSet<String>;

    /// Record a new fix attempt (pending status).
    fn record_attempt(&self, source: &str, issue_id: &str, short_id: &str) -> Result<()>;

    /// Record a new fix attempt with labels (pending status).
    fn record_attempt_with_labels(
        &self,
        source: &str,
        issue_id: &str,
        short_id: &str,
        labels: &[String],
    ) -> Result<()>;

    /// Update a fix attempt with success and PR URL.
    fn mark_success(&self, source: &str, issue_id: &str, pr_url: &str) -> Result<()>;

    /// Update a fix attempt with failure.
    fn mark_failed(&self, source: &str, issue_id: &str, error_message: &str) -> Result<()>;

    /// Mark a fix attempt as merged (PR was merged).
    fn mark_merged(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Mark a fix attempt as closed (PR was closed without merging).
    fn mark_closed(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Mark the issue as resolved on the remote source.
    fn mark_resolved(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Get a specific fix attempt.
    fn get_attempt(&self, source: &str, issue_id: &str) -> Result<Option<FixAttempt>>;

    /// Get all fix attempts with a specific status.
    fn get_attempts_by_status(&self, status: FixAttemptStatus) -> Result<Vec<FixAttempt>>;

    /// Get all successful attempts that haven't been merged yet (PRs to monitor).
    fn get_pending_prs(&self) -> Result<Vec<FixAttempt>>;

    /// Get a fix attempt by PR URL.
    fn get_attempt_by_pr_url(&self, pr_url: &str) -> Result<Option<FixAttempt>>;

    /// Reset a failed attempt to allow retry.
    fn reset_attempt(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Get statistics about fix attempts.
    fn get_stats(&self) -> Result<FixAttemptStats>;

    /// Increment retry count and set last retry timestamp.
    fn increment_retry(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Mark an issue as cannot be fixed (max retries reached).
    fn mark_cannot_fix(&self, source: &str, issue_id: &str, reason: &str) -> Result<()>;

    /// Get issues that are eligible for retry (failed/closed with retry_count < max_retries).
    fn get_retryable_issues(&self, max_retries: u32) -> Result<Vec<FixAttempt>>;

    /// Prepare an issue for retry (reset status to pending, clear PR info).
    fn prepare_for_retry(&self, source: &str, issue_id: &str) -> Result<()>;

    /// Record an activity log entry.
    fn record_activity(&self, _entry: &ActivityLogEntry) -> Result<i64> {
        Ok(0) // Default no-op
    }

    /// Get recent activity entries.
    fn get_recent_activities(&self, _limit: usize) -> Result<Vec<ActivityLogEntry>> {
        Ok(Vec::new()) // Default no-op
    }

    /// Record a Claude execution.
    fn record_execution(&self, _execution: &ClaudeExecution) -> Result<i64> {
        Ok(0) // Default no-op
    }

    /// Record a PR review.
    fn record_pr_review(&self, _review: &PrReviewRecord) -> Result<i64> {
        Ok(0) // Default no-op
    }

    /// Record an error pattern.
    fn record_error_pattern(&self, _pattern: &ErrorPattern) -> Result<i64> {
        Ok(0) // Default no-op
    }

    /// Record a processing metric.
    fn record_metric(&self, _metric: &ProcessingMetric) -> Result<i64> {
        Ok(0) // Default no-op
    }

    /// Get analytics summary.
    fn get_analytics_summary(&self) -> Result<AnalyticsSummary> {
        Ok(AnalyticsSummary::default())
    }

    /// Store a feedback outcome.
    fn store_feedback_outcome(&self, _outcome: &FixOutcome) -> Result<i64> {
        Ok(0)
    }

    /// Get feedback outcomes with optional source filter.
    fn get_feedback_outcomes(
        &self,
        _source: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<FixOutcome>> {
        Ok(Vec::new())
    }

    /// Get a feedback outcome by attempt ID.
    fn get_feedback_outcome_by_attempt(&self, _attempt_id: i64) -> Result<Option<FixOutcome>> {
        Ok(None)
    }

    // ─── Dashboard extension methods (default no-ops) ───

    /// Get recent activities with optional source filter.
    fn get_recent_activities_filtered(
        &self,
        _limit: usize,
        _source_filter: Option<&str>,
    ) -> Result<Vec<ActivityLogEntry>> {
        Ok(Vec::new())
    }

    /// Get a fix attempt by database ID.
    fn get_attempt_by_id(&self, _id: i64) -> Result<Option<FixAttempt>> {
        Ok(None)
    }

    /// Get Claude executions for a given attempt ID.
    fn get_executions_for_attempt(&self, _attempt_id: i64) -> Result<Vec<ClaudeExecution>> {
        Ok(Vec::new())
    }

    /// Get PR reviews for a given attempt ID.
    fn get_reviews_for_attempt(&self, _attempt_id: i64) -> Result<Vec<PrReviewRecord>> {
        Ok(Vec::new())
    }

    /// Get error patterns.
    fn get_error_patterns(&self, _limit: usize) -> Result<Vec<ErrorPattern>> {
        Ok(Vec::new())
    }

    /// Get processing metrics for a named metric.
    fn get_metrics(
        &self,
        _metric_name: &str,
        _since: Option<DateTime<Utc>>,
        _limit: usize,
    ) -> Result<Vec<ProcessingMetric>> {
        Ok(Vec::new())
    }

    /// Get open PRs from the prs table.
    fn get_open_prs(&self) -> Result<Vec<crate::types::PrRecord>> {
        Ok(Vec::new())
    }

    /// Get PR analytics.
    fn get_pr_analytics(&self) -> Result<PrAnalytics> {
        Ok(PrAnalytics::default())
    }

    /// Get regression watches by status.
    fn get_regression_watches_by_status(
        &self,
        _status: RegressionWatchStatus,
    ) -> Result<Vec<RegressionWatch>> {
        Ok(Vec::new())
    }

    /// Get all regression watches.
    fn get_all_regression_watches(&self) -> Result<Vec<RegressionWatch>> {
        Ok(Vec::new())
    }

    /// Get regression checks for a watch.
    fn get_regression_checks(&self, _watch_id: i64) -> Result<Vec<RegressionCheck>> {
        Ok(Vec::new())
    }

    /// Get active prompt experiments.
    fn get_active_experiments(&self) -> Result<Vec<PromptExperiment>> {
        Ok(Vec::new())
    }

    /// List indexed repositories.
    fn list_indexed_repos(&self) -> Result<Vec<StoredIndexedRepo>> {
        Ok(Vec::new())
    }

    /// Get index statistics.
    fn get_index_stats(&self) -> Result<IndexStats> {
        Ok(IndexStats {
            repo_count: 0,
            file_count: 0,
            last_indexed_at: None,
        })
    }

    /// List all dependencies.
    fn list_all_dependencies(&self) -> Result<Vec<StoredDependency>> {
        Ok(Vec::new())
    }

    /// Get inference statistics.
    fn get_inference_stats(&self) -> Result<InferenceStats> {
        Ok(InferenceStats {
            total_attempts: 0,
            with_feedback: 0,
            correct: 0,
            accuracy: 0.0,
            by_confidence: ConfidenceBreakdown {
                high: 0,
                medium: 0,
                low: 0,
                none: 0,
            },
        })
    }

    /// Get inference history.
    fn get_inference_history(&self, _limit: usize) -> Result<Vec<InferenceHistoryEntry>> {
        Ok(Vec::new())
    }

    /// List all PRs with optional status and limit filters.
    fn list_prs(
        &self,
        _status: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<crate::types::PrRecord>> {
        Ok(Vec::new())
    }
}

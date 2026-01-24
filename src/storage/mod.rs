//! Storage implementations for tracking fix attempts, embeddings, and analytics.

pub mod analytics;
mod sqlite;
pub mod vectorlite;

pub use analytics::{
    classify_error, compute_error_hash, AnalyticsService, TimePeriod, TrendAnalysis, TrendDirection,
};
pub use sqlite::{
    ConfidenceBreakdown, DiagnosticCounts, IndexStats, InferenceStats, SqliteTracker,
    StoredDependency, StoredIndexedRepo, StoredRepository,
};
pub use vectorlite::{is_vectorlite_available, try_load_vectorlite, VectorStoreConfig};

use crate::error::Result;
use crate::types::{
    ActivityLogEntry, AnalyticsSummary, ClaudeExecution, ErrorPattern, FixAttempt, FixAttemptStats,
    FixAttemptStatus, PrReviewRecord, ProcessingMetric,
};
use std::collections::HashSet;

/// Trait for tracking fix attempts.
pub trait FixAttemptTracker: Send + Sync {
    /// Check if an issue has already been attempted.
    fn has_attempted(&self, source: &str, issue_id: &str) -> bool;

    /// Get all attempted issue IDs for a source.
    fn get_attempted_issue_ids(&self, source: &str) -> HashSet<String>;

    /// Record a new fix attempt (pending status).
    fn record_attempt(&self, source: &str, issue_id: &str, short_id: &str) -> Result<()>;

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
}

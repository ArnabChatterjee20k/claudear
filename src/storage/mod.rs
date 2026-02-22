//! Storage implementations for tracking fix attempts, embeddings, and analytics.

pub mod analytics;
#[cfg(feature = "sqlite")]
pub(crate) mod migrator;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod tenant;
pub mod types;
#[cfg(feature = "sqlite")]
pub mod vectorlite;

pub use analytics::{
    classify_error, compute_error_hash, AnalyticsService, TimePeriod, TrendAnalysis, TrendDirection,
};
#[cfg(feature = "sqlite")]
pub use sqlite::SqliteTracker;
pub use types::{
    ConfidenceBreakdown, DiagnosticCounts, IndexStats, IndexingProgress, InferenceHistoryEntry,
    InferenceStats, StoredDependency, StoredIndexedRepo, StoredRepository, UserRow,
};
#[cfg(feature = "sqlite")]
pub use vectorlite::{is_vectorlite_available, try_load_vectorlite};

use crate::error::Result;
use crate::feedback::FixOutcome;
use crate::learning::cross_repo_correlator::CrossRepoCorrelation;
use crate::types::{
    ActivityLogEntry, AnalyticsSummary, AgentExecution, ErrorPattern, FixAttempt, FixAttemptStats,
    FixAttemptStatus, IssueEmbedding, PrAnalytics, PrRecord, PrReviewRecord, ProcessingMetric,
    PromptExperiment, QaKnowledgeEntry, QaMatch, RegressionCheck, RegressionWatch,
    RegressionWatchStatus, SimilarIssue,
};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Maximum allowed length for PR URLs to prevent ReDoS and excessive memory usage.
const MAX_PR_URL_LENGTH: usize = 2048;

/// Compiled regex for parsing GitHub PR URLs into repo/PR number.
static PR_URL_REGEX: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(r"github\.com/([^/]+/[^/]+)/pull/(\d+)")
        .expect("PR URL regex should be valid")
});

/// Compiled regex for parsing GitLab MR URLs.
static MR_URL_REGEX: LazyLock<regex_lite::Regex> = LazyLock::new(|| {
    regex_lite::Regex::new(r"https?://[^/]+/(.+?)/-/merge_requests/(\d+)")
        .expect("MR URL regex should be valid")
});

/// Parse a GitHub PR or GitLab MR URL into `(repo, number)`.
pub fn parse_pr_url(url: &str) -> Option<(String, i64)> {
    if url.len() > MAX_PR_URL_LENGTH {
        return None;
    }
    if let Some(caps) = PR_URL_REGEX.captures(url) {
        let repo = caps.get(1)?.as_str().to_string();
        let pr_number: i64 = caps.get(2)?.as_str().parse().ok()?;
        return Some((repo, pr_number));
    }
    if let Some(caps) = MR_URL_REGEX.captures(url) {
        let project = caps.get(1)?.as_str().to_string();
        let mr_iid: i64 = caps.get(2)?.as_str().parse().ok()?;
        return Some((project, mr_iid));
    }
    None
}

/// Trait for tracking fix attempts.
pub trait FixAttemptTracker: Send + Sync {
    /// Check if an issue has already been attempted.
    fn has_attempted(&self, source: &str, issue_id: &str) -> Result<bool>;

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

    /// Record an agent execution.
    fn record_execution(&self, _execution: &AgentExecution) -> Result<i64> {
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

    /// Store a Q&A knowledge entry.
    fn store_qa_knowledge(&self, _entry: &QaKnowledgeEntry) -> Result<i64> {
        Ok(0)
    }

    /// Find semantically similar Q&A entries within a scoped source/repo.
    fn find_similar_qa_scoped(
        &self,
        _source: &str,
        _repo: Option<&str>,
        _question_norm: &str,
        _question_embedding: Option<&[f32]>,
        _threshold: f64,
        _limit: usize,
    ) -> Result<Vec<QaMatch>> {
        Ok(Vec::new())
    }

    /// Find semantically similar Q&A entries across all sources.
    fn find_similar_qa_global(
        &self,
        _question_norm: &str,
        _question_embedding: Option<&[f32]>,
        _threshold: f64,
        _limit: usize,
    ) -> Result<Vec<QaMatch>> {
        Ok(Vec::new())
    }

    /// Record usage of a Q&A entry for an attempt.
    fn record_qa_usage(
        &self,
        _attempt_id: i64,
        _qa_id: i64,
        _usage_type: &str,
        _similarity_score: f64,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Update success/failure counters for a Q&A entry.
    fn update_qa_outcome_stats(&self, _qa_id: i64, _success: bool) -> Result<()> {
        Ok(())
    }

    /// Update Q&A outcome counters for all Q&A usage linked to an attempt.
    fn update_qa_outcome_stats_for_attempt(&self, _attempt_id: i64, _success: bool) -> Result<()> {
        Ok(())
    }

    /// Retrieve a channel cursor value.
    fn get_channel_cursor(&self, _channel: &str, _cursor_key: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Save a channel cursor value.
    fn set_channel_cursor(
        &self,
        _channel: &str,
        _cursor_key: &str,
        _cursor_value: &str,
    ) -> Result<()> {
        Ok(())
    }

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

    /// Get agent executions for a given attempt ID.
    fn get_executions_for_attempt(&self, _attempt_id: i64) -> Result<Vec<AgentExecution>> {
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

    /// Average time from issue attempt to PR creation in minutes.
    fn get_avg_time_to_pr(&self) -> Result<Option<f64>> {
        Ok(None)
    }

    /// Top rejection/review-change reason categories.
    fn get_rejection_reasons(&self, _limit: usize) -> Result<Vec<crate::types::RejectionReason>> {
        Ok(Vec::new())
    }

    /// Count Claude agent spawns since a given ISO timestamp.
    fn get_agent_spawn_count(&self, _since_iso: &str) -> Result<i64> {
        Ok(0)
    }

    /// Compute cost estimate from Claude execution data.
    fn get_cost_estimate(
        &self,
        _since_iso: &str,
        _max_plan_monthly_cost: f64,
        _period_label: &str,
    ) -> Result<crate::types::CostEstimate> {
        Ok(crate::types::CostEstimate::default())
    }

    /// MTTR trend grouped by week.
    fn get_mttr_trend(&self, _weeks: usize) -> Result<Vec<crate::types::MttrDataPoint>> {
        Ok(Vec::new())
    }

    /// Per-repository leaderboard.
    fn get_repo_leaderboard(&self) -> Result<Vec<crate::types::RepoLeaderboardEntry>> {
        Ok(Vec::new())
    }

    /// Complexity-based engineering time savings estimate.
    fn get_complexity_time_savings(
        &self,
        _since_iso: &str,
        _hourly_rate: f64,
        _period_label: &str,
    ) -> Result<crate::types::TimeSavings> {
        Ok(crate::types::TimeSavings::default())
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

    /// Get current indexing progress.
    fn get_indexing_progress(&self) -> Result<IndexingProgress> {
        Ok(IndexingProgress {
            status: "idle".to_string(),
            total_repos: 0,
            indexed_repos: 0,
            current_repo: None,
            current_repo_files: 0,
            total_files_indexed: 0,
            started_at: None,
            updated_at: None,
        })
    }

    /// Subscribe to real-time indexing progress updates via a watch channel.
    /// Default implementation returns a receiver that never changes (dead channel).
    fn subscribe_indexing_progress(&self) -> tokio::sync::watch::Receiver<IndexingProgress> {
        let (_, rx) = tokio::sync::watch::channel(IndexingProgress::default());
        rx
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

    /// System 1: Update learnings text on a feedback outcome.
    fn update_feedback_learnings(&self, _outcome_id: i64, _learnings: &str) -> Result<()> {
        Ok(())
    }

    /// System 2: Store a diff analysis for a merged PR.
    fn store_diff_analysis(&self, _analysis: &crate::types::DiffAnalysis) -> Result<i64> {
        Ok(0)
    }

    /// System 2: Get diff analyses for a repo.
    fn get_diff_analyses_for_repo(
        &self,
        _repo: &str,
        _limit: usize,
    ) -> Result<Vec<crate::types::DiffAnalysis>> {
        Ok(Vec::new())
    }

    /// System 3: Upsert a promoted instruction.
    fn upsert_promoted_instruction(
        &self,
        _instruction: &crate::types::PromotedInstruction,
    ) -> Result<i64> {
        Ok(0)
    }

    /// System 3: Get active promoted instructions for a repo.
    fn get_promoted_instructions(
        &self,
        _repo: &str,
    ) -> Result<Vec<crate::types::PromotedInstruction>> {
        Ok(Vec::new())
    }

    /// System 4: Upsert a repo knowledge entry.
    fn upsert_repo_knowledge(&self, _entry: &crate::types::RepoKnowledge) -> Result<i64> {
        Ok(0)
    }

    /// System 4: Get all knowledge for a repo.
    fn get_repo_knowledge(&self, _repo: &str) -> Result<Vec<crate::types::RepoKnowledge>> {
        Ok(Vec::new())
    }

    /// System 4: Get repo knowledge by key.
    fn get_repo_knowledge_by_key(
        &self,
        _repo: &str,
        _key: &str,
    ) -> Result<Vec<crate::types::RepoKnowledge>> {
        Ok(Vec::new())
    }

    /// System 5: Upsert a review pattern.
    fn upsert_review_pattern(&self, _pattern: &crate::types::ReviewPattern) -> Result<i64> {
        Ok(0)
    }

    /// System 5: Get review patterns for a repo.
    fn get_review_patterns(
        &self,
        _repo: &str,
        _limit: usize,
    ) -> Result<Vec<crate::types::ReviewPattern>> {
        Ok(Vec::new())
    }

    /// System 5: Get review patterns by category.
    fn get_review_patterns_by_category(
        &self,
        _repo: &str,
        _category: crate::types::ReviewCategory,
    ) -> Result<Vec<crate::types::ReviewPattern>> {
        Ok(Vec::new())
    }

    /// System 6: Store a strategy fingerprint.
    fn store_strategy_fingerprint(
        &self,
        _fingerprint: &crate::types::StrategyFingerprint,
    ) -> Result<i64> {
        Ok(0)
    }

    /// System 6: Get successful strategies for a repo.
    fn get_successful_strategies(
        &self,
        _repo: &str,
        _limit: usize,
    ) -> Result<Vec<crate::types::StrategyFingerprint>> {
        Ok(Vec::new())
    }

    /// System 7: Update a PR's fix quality score.
    fn update_pr_fix_quality_score(&self, _pr_url: &str, _score: f64) -> Result<()> {
        Ok(())
    }

    /// System 8: Store an issue cluster.
    fn store_issue_cluster(&self, _cluster: &crate::types::IssueCluster) -> Result<i64> {
        Ok(0)
    }

    /// System 8: Get active (unresolved) clusters for a source.
    fn get_active_clusters(&self, _source: &str) -> Result<Vec<crate::types::IssueCluster>> {
        Ok(Vec::new())
    }

    /// System 8: Mark a cluster as resolved.
    fn update_cluster_resolution(
        &self,
        _cluster_id: i64,
        _resolved_by_issue_id: &str,
        _resolved_by_attempt_id: i64,
    ) -> Result<()> {
        Ok(())
    }

    /// System 8: Get recent issue arrivals within a time window.
    fn get_recent_issue_arrivals(
        &self,
        _source: &str,
        _window_minutes: i64,
    ) -> Result<Vec<(String, DateTime<Utc>)>> {
        Ok(Vec::new())
    }

    /// Store a content cluster detected by the prioritisation engine.
    fn store_content_cluster(&self, _cluster: &crate::types::ContentCluster) -> Result<i64> {
        Ok(0)
    }

    /// Get active (unresolved) content clusters for a source.
    fn get_active_content_clusters(
        &self,
        _source: &str,
    ) -> Result<Vec<crate::types::ContentCluster>> {
        Ok(Vec::new())
    }

    /// Resolve a content cluster by its database ID.
    fn resolve_content_cluster(&self, _cluster_id: i64) -> Result<()> {
        Ok(())
    }

    /// Store the severity score and blast radius for an issue.
    fn store_severity_score(
        &self,
        _source: &str,
        _issue_id: &str,
        _score: &crate::types::SeverityScore,
        _blast_radius: crate::types::BlastRadius,
    ) -> Result<()> {
        Ok(())
    }

    /// Record that an issue was suppressed by a rule.
    fn record_suppression(
        &self,
        _source: &str,
        _issue_id: &str,
        _rule_name: &str,
        _reason: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Get recent fix attempts since a cutoff time.
    fn get_recent_attempts_since(&self, _since: &DateTime<Utc>) -> Result<Vec<FixAttempt>> {
        Ok(Vec::new())
    }

    /// Check if repo_a depends on repo_b.
    fn has_dependency(&self, _repo_a: &str, _repo_b: &str) -> Result<bool> {
        Ok(false)
    }

    /// Upsert a cross-repo correlation record, incrementing the count.
    fn upsert_cross_repo_correlation(
        &self,
        _repo_a: &str,
        _repo_b: &str,
        _window_hours: i64,
    ) -> Result<CrossRepoCorrelation> {
        Ok(CrossRepoCorrelation {
            id: 0,
            repo_a: _repo_a.to_string(),
            repo_b: _repo_b.to_string(),
            correlation_count: 0,
            last_seen_at: Utc::now(),
            window_hours: _window_hours,
        })
    }

    /// Get cross-repo correlations above a minimum count and within max age.
    fn get_cross_repo_correlations(
        &self,
        _min_count: i64,
        _max_age_hours: i64,
    ) -> Result<Vec<CrossRepoCorrelation>> {
        Ok(Vec::new())
    }

    // --- Auth: User Management ---

    /// Create a new user.
    fn create_user(
        &self,
        _email: &str,
        _password_hash: &str,
        _name: &str,
        _role: &str,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Get a user by email.
    fn get_user_by_email(&self, _email: &str) -> Result<Option<UserRow>> {
        Ok(None)
    }

    /// Get a user by ID.
    fn get_user_by_id(&self, _id: i64) -> Result<Option<UserRow>> {
        Ok(None)
    }

    /// List all users.
    fn list_users(&self) -> Result<Vec<UserRow>> {
        Ok(Vec::new())
    }

    /// Update user fields. Returns true if a row was modified.
    fn update_user(
        &self,
        _id: i64,
        _email: Option<&str>,
        _password_hash: Option<&str>,
        _name: Option<&str>,
        _role: Option<&str>,
        _avatar_url: Option<&str>,
    ) -> Result<bool> {
        Ok(false)
    }

    /// Delete a user by ID. Returns true if a row was deleted.
    fn delete_user(&self, _id: i64) -> Result<bool> {
        Ok(false)
    }

    /// Count total users.
    fn count_users(&self) -> Result<i64> {
        Ok(0)
    }

    // --- Auth: Session Management ---

    /// Create a session for a user, returning the session token.
    fn create_session(&self, _user_id: i64, _expires_at: &str) -> Result<String> {
        Ok(String::new())
    }

    /// Get the user associated with a session token.
    fn get_session_user(&self, _token: &str) -> Result<Option<UserRow>> {
        Ok(None)
    }

    /// Delete a session by token.
    fn delete_session(&self, _token: &str) -> Result<()> {
        Ok(())
    }

    /// Cleanup expired sessions, returning how many were removed.
    fn cleanup_expired_sessions(&self) -> Result<usize> {
        Ok(0)
    }

    /// Delete all sessions for a user.
    fn delete_user_sessions(&self, _user_id: i64) -> Result<()> {
        Ok(())
    }

    // --- Fix Attempts: Listing & Counting ---

    /// List fix attempts with optional filters and pagination.
    fn list_attempts(
        &self,
        _status: Option<&str>,
        _source: Option<&str>,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<FixAttempt>> {
        Ok(Vec::new())
    }

    /// Count fix attempts with optional filters.
    fn count_attempts(&self, _status: Option<&str>, _source: Option<&str>) -> Result<usize> {
        Ok(0)
    }

    /// List the most recent fix attempts.
    fn list_recent_attempts(&self, _limit: usize) -> Result<Vec<FixAttempt>> {
        Ok(Vec::new())
    }

    /// List fix attempts created since a timestamp.
    fn list_attempts_since(&self, _since: DateTime<Utc>) -> Result<Vec<FixAttempt>> {
        Ok(Vec::new())
    }

    /// Get the most recent merged attempt for a given SCM repo.
    fn get_most_recent_merged_attempt_for_repo(
        &self,
        _scm_repo: &str,
    ) -> Result<Option<FixAttempt>> {
        Ok(None)
    }

    // --- PR Lifecycle ---

    /// Upsert a PR record. Returns the row ID.
    fn upsert_pr(&self, _pr: &PrRecord) -> Result<i64> {
        Ok(0)
    }

    /// Get a PR record by URL.
    fn get_pr(&self, _pr_url: &str) -> Result<Option<PrRecord>> {
        Ok(None)
    }

    /// Update a PR's status.
    fn update_pr_status(&self, _pr_url: &str, _status: &str) -> Result<()> {
        Ok(())
    }

    // --- Issue Management ---

    /// List issues with optional source filter and pagination.
    fn list_issues(
        &self,
        _source: Option<&str>,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<IssueEmbedding>> {
        Ok(Vec::new())
    }

    /// Count issues with optional source filter.
    fn count_issues(&self, _source: Option<&str>) -> Result<usize> {
        Ok(0)
    }

    /// Store an issue embedding record. Returns the row ID.
    fn store_issue(&self, _issue: &IssueEmbedding) -> Result<i64> {
        Ok(0)
    }

    // --- Executions ---

    /// Get a specific execution for an attempt by execution ID.
    fn get_execution_for_attempt(
        &self,
        _attempt_id: i64,
        _execution_id: i64,
    ) -> Result<Option<AgentExecution>> {
        Ok(None)
    }

    // --- Webhook Deduplication ---

    /// Check if a delivery has been seen and record it. Returns true if already seen.
    fn check_and_record_delivery(&self, _delivery_id: &str, _source: &str) -> Result<bool> {
        Ok(false)
    }

    /// Cleanup old deliveries older than max_age_hours. Returns count removed.
    fn cleanup_old_deliveries(&self, _max_age_hours: u64) -> Result<usize> {
        Ok(0)
    }

    // --- Repository Sync ---

    /// Sync repositories from a RepoIndex into storage. Returns count synced.
    fn sync_from_index(
        &self,
        _index: &crate::repo::RepoIndex,
        _sync_files: bool,
    ) -> Result<usize> {
        Ok(0)
    }

    /// Sync a single repository's files into storage.
    fn sync_repo_files(&self, _repo: &crate::repo::IndexedRepo) -> Result<()> {
        Ok(())
    }

    // --- Cascade Attempts ---

    /// Record a cascade fix attempt. Returns the new attempt ID.
    fn record_cascade_attempt(
        &self,
        _source: &str,
        _issue_id: &str,
        _short_id: &str,
        _parent_attempt_id: i64,
        _cascade_repo: &str,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Update a fix attempt with its PR details.
    fn update_attempt_pr(
        &self,
        _attempt_id: i64,
        _pr_url: &str,
        _scm_repo: &str,
        _pr_number: i64,
    ) -> Result<()> {
        Ok(())
    }

    /// Mark a cascade attempt as failed.
    fn mark_cascade_failed(&self, _attempt_id: i64, _error: &str) -> Result<()> {
        Ok(())
    }

    // --- Regression Watching ---

    /// Create a regression watch. Returns the watch ID.
    fn create_regression_watch(&self, _watch: &RegressionWatch) -> Result<i64> {
        Ok(0)
    }

    /// Update a regression watch's status.
    fn update_regression_watch_status(
        &self,
        _id: i64,
        _status: RegressionWatchStatus,
    ) -> Result<()> {
        Ok(())
    }

    /// Get a regression watch by ID.
    fn get_regression_watch(&self, _id: i64) -> Result<Option<RegressionWatch>> {
        Ok(None)
    }

    /// Record a regression check. Returns the check ID.
    fn record_regression_check(&self, _check: &RegressionCheck) -> Result<i64> {
        Ok(0)
    }

    // --- PR Review State ---

    /// Save a PR review state.
    fn save_pr_review_state(&self, _state: &crate::scm::PrReviewState) -> Result<()> {
        Ok(())
    }

    /// Get all active PR review states.
    fn get_active_pr_review_states(&self) -> Result<Vec<crate::scm::PrReviewState>> {
        Ok(Vec::new())
    }

    /// Deactivate a PR review state.
    fn deactivate_pr_review_state(&self, _pr_url: &str) -> Result<()> {
        Ok(())
    }

    /// Record a PR review comment. Returns the row ID.
    fn record_pr_review_comment(
        &self,
        _pr_url: &str,
        _comment: &crate::scm::ReviewComment,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Get all stored review comments for a PR.
    fn get_comments_for_pr(
        &self,
        _pr_url: &str,
    ) -> Result<Vec<types::StoredPrReviewComment>> {
        Ok(Vec::new())
    }

    // --- Evaluation ---

    /// Store an evaluation snapshot. Returns the row ID.
    fn store_eval_snapshot(
        &self,
        _attempt_id: Option<i64>,
        _phase: &str,
        _snapshot: &crate::evaluation::EvalSnapshot,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Store an evaluation delta. Returns the row ID.
    fn store_eval_delta(
        &self,
        _attempt_id: Option<i64>,
        _repo: &str,
        _delta: &crate::evaluation::EvalDelta,
    ) -> Result<i64> {
        Ok(0)
    }

    // --- Batch Activity & Pruning ---

    /// Record a batch of activity log entries. Returns count recorded.
    fn record_activities_batch(&self, _entries: &[ActivityLogEntry]) -> Result<usize> {
        Ok(0)
    }

    /// Prune old activity log entries. Returns count pruned.
    fn prune_old_activities(&self, _days_to_keep: i64) -> Result<usize> {
        Ok(0)
    }

    /// Prune old processing metrics. Returns count pruned.
    fn prune_old_metrics(&self, _days_to_keep: i64) -> Result<usize> {
        Ok(0)
    }

    // --- Metrics & Diagnostics ---

    /// Get activity type counts since a timestamp.
    fn get_activity_type_counts_since(
        &self,
        _since: DateTime<Utc>,
    ) -> Result<HashMap<String, i64>> {
        Ok(HashMap::new())
    }

    /// Get metric counts for named metrics since a timestamp.
    fn get_metric_counts_since(
        &self,
        _metric_names: &[&str],
        _since: DateTime<Utc>,
    ) -> Result<HashMap<String, i64>> {
        Ok(HashMap::new())
    }

    /// Get metric sums for named metrics since a timestamp.
    fn get_metric_sums_since(
        &self,
        _metric_names: &[&str],
        _since: DateTime<Utc>,
    ) -> Result<HashMap<String, f64>> {
        Ok(HashMap::new())
    }

    /// Get metric sums grouped by source for named metrics since a timestamp.
    fn get_metric_sums_by_source_since(
        &self,
        _metric_names: &[&str],
        _since: DateTime<Utc>,
    ) -> Result<HashMap<(String, String), f64>> {
        Ok(HashMap::new())
    }

    /// Get diagnostic row counts for all major tables.
    fn get_diagnostic_counts(&self) -> Result<DiagnosticCounts> {
        Ok(DiagnosticCounts {
            fix_attempts: 0,
            fix_attempts_by_status: HashMap::new(),
            activity_log: 0,
            claude_executions: 0,
            pr_reviews: 0,
            pr_review_states: 0,
            issues: 0,
            similar_issues: 0,
            repositories: 0,
            repo_files: 0,
            inference_attempts: 0,
            error_patterns: 0,
            processing_metrics: 0,
            feedback_outcomes: 0,
            prs: 0,
            recent_fix_attempts: Vec::new(),
        })
    }

    // --- Inference ---

    /// Record an inference attempt. Returns the row ID.
    fn record_inference_attempt(
        &self,
        _issue_id: &str,
        _issue_source: &str,
        _extracted_filenames: &[String],
        _extracted_functions: &[String],
        _extracted_keywords: &[String],
        _inferred_repo_id: Option<i64>,
        _confidence: &str,
        _inference_reason: &str,
        _duration_ms: Option<u64>,
    ) -> Result<i64> {
        Ok(0)
    }

    /// Record feedback on an inference attempt.
    fn record_inference_feedback(
        &self,
        _inference_id: i64,
        _was_correct: bool,
        _actual_repo_id: Option<i64>,
        _feedback_source: &str,
    ) -> Result<()> {
        Ok(())
    }

    // --- Embeddings ---

    /// Store a single issue embedding. Returns the row ID.
    fn store_embedding(&self, _embedding: &IssueEmbedding) -> Result<i64> {
        Ok(0)
    }

    /// Store a batch of issue embeddings.
    fn store_embeddings_batch(&self, _embeddings: &[IssueEmbedding]) -> Result<()> {
        Ok(())
    }

    /// Get an embedding by source and issue ID.
    fn get_embedding(
        &self,
        _source: &str,
        _issue_id: &str,
    ) -> Result<Option<IssueEmbedding>> {
        Ok(None)
    }

    /// Get all embeddings with optional source filter and pagination.
    fn get_all_embeddings(
        &self,
        _source: Option<&str>,
        _limit: Option<usize>,
        _offset: Option<usize>,
    ) -> Result<Vec<IssueEmbedding>> {
        Ok(Vec::new())
    }

    /// Find similar issues by vector similarity. Returns None if vector search is unavailable.
    fn find_similar_issues_vector(
        &self,
        _query_embedding: &[f32],
        _source: &str,
        _exclude_issue_id: Option<&str>,
        _min_similarity: f64,
        _limit: usize,
    ) -> Result<Option<Vec<(IssueEmbedding, f64)>>> {
        Ok(None)
    }

    /// Find similar outcomes by vector similarity. Returns None if vector search is unavailable.
    fn find_similar_outcomes_vector(
        &self,
        _query_embedding: &[f32],
        _min_similarity: f64,
        _limit: usize,
    ) -> Result<Option<Vec<(FixOutcome, f64)>>> {
        Ok(None)
    }

    // --- Experiments ---

    /// Save a prompt experiment. Returns the row ID.
    fn save_experiment(&self, _experiment: &PromptExperiment) -> Result<i64> {
        Ok(0)
    }

    /// Update experiment stats after a result.
    fn update_experiment_stats(
        &self,
        _experiment_id: i64,
        _success: bool,
        _time_to_merge: Option<f64>,
    ) -> Result<()> {
        Ok(())
    }

    // --- Batch Operations ---

    /// Get fix attempts for a batch of (source, issue_id) keys.
    fn get_attempts_batch(&self, _keys: &[(&str, &str)]) -> Result<Vec<Option<FixAttempt>>> {
        Ok(Vec::new())
    }

    /// Store a batch of similar issue records.
    fn store_similar_issues_batch(&self, _similar_issues: &[SimilarIssue]) -> Result<()> {
        Ok(())
    }

    // --- Release Tracking ---

    /// Record a release tracking entry. Returns the row ID.
    fn record_release_tracking(
        &self,
        _tracking: &crate::types::ReleaseTracking,
    ) -> Result<i64> {
        Ok(0)
    }

    // --- Repository Lookup ---

    /// Get an indexed repository by name.
    fn get_indexed_repo(&self, _name: &str) -> Result<Option<StoredIndexedRepo>> {
        Ok(None)
    }

    // --- Analytics ---

    /// Get the overall success rate (0.0-1.0).
    fn get_success_rate(&self) -> Result<f64> {
        Ok(0.0)
    }

    // --- Code Indexing ---

    /// Get or create a repository ID by name.
    fn get_or_create_repo_id(&self, _name: &str) -> Result<i64> {
        Ok(0)
    }

    /// Check if a file's content hash matches what's already indexed.
    fn code_chunk_hash_matches(
        &self,
        _repo_id: i64,
        _file_path: &str,
        _file_hash: &str,
    ) -> Result<bool> {
        Ok(false)
    }

    /// Delete all code symbols, chunks, and embeddings for a specific file.
    fn delete_code_data_for_file(&self, _repo_id: i64, _file_path: &str) -> Result<()> {
        Ok(())
    }

    /// Delete code chunks by IDs.
    fn delete_code_chunks_by_ids(&self, _chunk_ids: &[i64]) -> Result<()> {
        Ok(())
    }

    /// Remove code data for files that no longer exist in the repository.
    fn cleanup_stale_code_data(&self, _repo_id: i64, _current_paths: &[String]) -> Result<()> {
        Ok(())
    }

    /// Batch-save extracted code symbols.
    fn save_code_symbols(
        &self,
        _symbols: &[crate::repo::code_index::CodeSymbol],
    ) -> Result<()> {
        Ok(())
    }

    /// Batch-save code chunks. Returns the assigned IDs.
    fn save_code_chunks(
        &self,
        _chunks: &[crate::repo::code_index::CodeChunk],
    ) -> Result<Vec<i64>> {
        Ok(Vec::new())
    }

    /// Save embeddings for code chunks.
    fn save_code_chunk_embeddings(
        &self,
        _pairs: &[(i64, &[f32])],
        _model_name: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Search code chunks by vector similarity.
    fn search_code_chunks(
        &self,
        _query_embedding: &[f32],
        _repo_id: Option<i64>,
        _limit: usize,
    ) -> Result<Vec<crate::repo::code_index::CodeSearchResult>> {
        Ok(Vec::new())
    }

    /// Find code symbols by name substring.
    fn find_code_symbols(
        &self,
        _name: &str,
        _kind: Option<crate::repo::code_index::SymbolKind>,
        _repo_id: Option<i64>,
    ) -> Result<Vec<crate::repo::code_index::CodeSymbol>> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal struct implementing only the required trait methods
    /// to verify that all default methods return correct no-op values.
    struct NoOpTracker;

    impl FixAttemptTracker for NoOpTracker {
        fn has_attempted(&self, _: &str, _: &str) -> Result<bool> {
            Ok(false)
        }
        fn get_attempted_issue_ids(&self, _: &str) -> HashSet<String> {
            HashSet::new()
        }
        fn record_attempt(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn record_attempt_with_labels(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: &[String],
        ) -> Result<()> {
            Ok(())
        }
        fn mark_success(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn mark_failed(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn mark_merged(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn mark_closed(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn mark_resolved(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn get_attempt(&self, _: &str, _: &str) -> Result<Option<FixAttempt>> {
            Ok(None)
        }
        fn get_attempts_by_status(&self, _: FixAttemptStatus) -> Result<Vec<FixAttempt>> {
            Ok(vec![])
        }
        fn get_pending_prs(&self) -> Result<Vec<FixAttempt>> {
            Ok(vec![])
        }
        fn get_attempt_by_pr_url(&self, _: &str) -> Result<Option<FixAttempt>> {
            Ok(None)
        }
        fn reset_attempt(&self, _: &str, _: &str) -> Result<()> {
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
        fn increment_retry(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn mark_cannot_fix(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn get_retryable_issues(&self, _: u32) -> Result<Vec<FixAttempt>> {
            Ok(vec![])
        }
        fn prepare_for_retry(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_default_record_activity_returns_zero() {
        let t = NoOpTracker;
        let entry = ActivityLogEntry::new("test", "message");
        assert_eq!(t.record_activity(&entry).unwrap(), 0);
    }

    #[test]
    fn test_default_get_recent_activities_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_recent_activities(100).unwrap().is_empty());
    }

    #[test]
    fn test_default_record_execution_returns_zero() {
        let t = NoOpTracker;
        let exec = AgentExecution::new();
        assert_eq!(t.record_execution(&exec).unwrap(), 0);
    }

    #[test]
    fn test_default_get_analytics_summary_returns_default() {
        let t = NoOpTracker;
        let summary = t.get_analytics_summary().unwrap();
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn test_default_store_feedback_outcome_returns_zero() {
        let t = NoOpTracker;
        let outcome = FixOutcome {
            id: 0,
            attempt_id: 0,
            source: "s".into(),
            issue_id: "i".into(),
            issue_text: "t".into(),
            prompt_used: "p".into(),
            outcome: crate::feedback::Outcome::Merged,
            error_type: None,
            learnings: None,
            keywords: vec![],
            embedding: None,
            created_at: Utc::now(),
        };
        assert_eq!(t.store_feedback_outcome(&outcome).unwrap(), 0);
    }

    #[test]
    fn test_default_get_feedback_outcomes_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_feedback_outcomes(None, 10).unwrap().is_empty());
        assert!(t
            .get_feedback_outcomes(Some("linear"), 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_get_feedback_outcome_by_attempt_returns_none() {
        let t = NoOpTracker;
        assert!(t.get_feedback_outcome_by_attempt(999).unwrap().is_none());
    }

    #[test]
    fn test_default_store_qa_knowledge_returns_zero() {
        let t = NoOpTracker;
        let entry = QaKnowledgeEntry {
            id: 0,
            source: "s".into(),
            repo: None,
            issue_id: "i".into(),
            short_id: "S".into(),
            question_text: "q".into(),
            question_norm: "q".into(),
            question_embedding: None,
            answer_text: "a".into(),
            answer_norm: "a".into(),
            answer_embedding: None,
            channel: "c".into(),
            responder: None,
            correlation_id: "x".into(),
            asked_at: Utc::now(),
            answered_at: Utc::now(),
            success_count: 0,
            failure_count: 0,
            last_used_at: None,
            metadata: None,
        };
        assert_eq!(t.store_qa_knowledge(&entry).unwrap(), 0);
    }

    #[test]
    fn test_default_find_similar_qa_returns_empty() {
        let t = NoOpTracker;
        assert!(t
            .find_similar_qa_scoped("s", None, "q", None, 0.0, 10)
            .unwrap()
            .is_empty());
        assert!(t
            .find_similar_qa_global("q", None, 0.0, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_channel_cursor_returns_none() {
        let t = NoOpTracker;
        assert!(t.get_channel_cursor("ch", "key").unwrap().is_none());
        // set_channel_cursor should succeed silently
        assert!(t.set_channel_cursor("ch", "key", "val").is_ok());
    }

    #[test]
    fn test_default_get_open_prs_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_open_prs().unwrap().is_empty());
    }

    #[test]
    fn test_default_get_pr_analytics_returns_default() {
        let t = NoOpTracker;
        let analytics = t.get_pr_analytics().unwrap();
        assert_eq!(analytics.total, 0);
    }

    #[test]
    fn test_default_get_regression_watches_returns_empty() {
        let t = NoOpTracker;
        assert!(t
            .get_regression_watches_by_status(RegressionWatchStatus::AwaitingRelease)
            .unwrap()
            .is_empty());
        assert!(t.get_all_regression_watches().unwrap().is_empty());
    }

    #[test]
    fn test_default_get_active_experiments_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_active_experiments().unwrap().is_empty());
    }

    #[test]
    fn test_default_learning_methods_return_no_ops() {
        let t = NoOpTracker;
        assert!(t.update_feedback_learnings(1, "learnings").is_ok());
        assert!(t.get_diff_analyses_for_repo("repo", 10).unwrap().is_empty());
        assert!(t.get_promoted_instructions("repo").unwrap().is_empty());
        assert!(t.get_repo_knowledge("repo").unwrap().is_empty());
        assert!(t
            .get_repo_knowledge_by_key("repo", "key")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_review_pattern_methods_return_no_ops() {
        let t = NoOpTracker;
        assert!(t.get_review_patterns("repo", 10).unwrap().is_empty());
        assert!(t
            .get_review_patterns_by_category("repo", crate::types::ReviewCategory::Other)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_strategy_methods_return_no_ops() {
        let t = NoOpTracker;
        assert!(t.get_successful_strategies("repo", 10).unwrap().is_empty());
    }

    #[test]
    fn test_default_cluster_methods_return_no_ops() {
        let t = NoOpTracker;
        assert!(t.get_active_clusters("sentry").unwrap().is_empty());
        assert!(t.update_cluster_resolution(1, "issue", 1).is_ok());
        assert!(t
            .get_recent_issue_arrivals("sentry", 30)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_quality_score_update_succeeds() {
        let t = NoOpTracker;
        assert!(t.update_pr_fix_quality_score("url", 0.9).is_ok());
    }

    #[test]
    fn test_default_index_stats_returns_zeros() {
        let t = NoOpTracker;
        let stats = t.get_index_stats().unwrap();
        assert_eq!(stats.repo_count, 0);
        assert_eq!(stats.file_count, 0);
        assert!(stats.last_indexed_at.is_none());
    }

    #[test]
    fn test_default_inference_stats_returns_zeros() {
        let t = NoOpTracker;
        let stats = t.get_inference_stats().unwrap();
        assert_eq!(stats.total_attempts, 0);
        assert_eq!(stats.accuracy, 0.0);
    }

    #[test]
    fn test_default_record_pr_review_returns_zero() {
        let t = NoOpTracker;
        let review = PrReviewRecord::new("https://github.com/org/repo/pull/1");
        assert_eq!(t.record_pr_review(&review).unwrap(), 0);
    }

    #[test]
    fn test_default_record_error_pattern_returns_zero() {
        let t = NoOpTracker;
        let pattern = ErrorPattern::new("hash123");
        assert_eq!(t.record_error_pattern(&pattern).unwrap(), 0);
    }

    #[test]
    fn test_default_record_metric_returns_zero() {
        let t = NoOpTracker;
        let metric = ProcessingMetric::new("queue_depth", 42.0);
        assert_eq!(t.record_metric(&metric).unwrap(), 0);
    }

    #[test]
    fn test_default_record_qa_usage_returns_zero() {
        let t = NoOpTracker;
        assert_eq!(t.record_qa_usage(1, 2, "direct", 0.95).unwrap(), 0);
    }

    #[test]
    fn test_default_update_qa_outcome_stats_succeeds() {
        let t = NoOpTracker;
        assert!(t.update_qa_outcome_stats(1, true).is_ok());
        assert!(t.update_qa_outcome_stats(2, false).is_ok());
    }

    #[test]
    fn test_default_update_qa_outcome_stats_for_attempt_succeeds() {
        let t = NoOpTracker;
        assert!(t.update_qa_outcome_stats_for_attempt(1, true).is_ok());
        assert!(t.update_qa_outcome_stats_for_attempt(2, false).is_ok());
    }

    #[test]
    fn test_default_get_recent_activities_filtered_returns_empty() {
        let t = NoOpTracker;
        assert!(t
            .get_recent_activities_filtered(50, None)
            .unwrap()
            .is_empty());
        assert!(t
            .get_recent_activities_filtered(50, Some("linear"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_get_attempt_by_id_returns_none() {
        let t = NoOpTracker;
        assert!(t.get_attempt_by_id(1).unwrap().is_none());
        assert!(t.get_attempt_by_id(999).unwrap().is_none());
    }

    #[test]
    fn test_default_get_executions_for_attempt_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_executions_for_attempt(1).unwrap().is_empty());
    }

    #[test]
    fn test_default_get_reviews_for_attempt_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_reviews_for_attempt(1).unwrap().is_empty());
    }

    #[test]
    fn test_default_get_error_patterns_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_error_patterns(100).unwrap().is_empty());
    }

    #[test]
    fn test_default_get_metrics_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_metrics("queue_depth", None, 100).unwrap().is_empty());
        assert!(t
            .get_metrics("processing_time", Some(Utc::now()), 50)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_default_get_avg_time_to_pr_returns_none() {
        let t = NoOpTracker;
        assert!(t.get_avg_time_to_pr().unwrap().is_none());
    }

    #[test]
    fn test_default_get_rejection_reasons_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_rejection_reasons(10).unwrap().is_empty());
    }

    #[test]
    fn test_default_get_agent_spawn_count_returns_zero() {
        let t = NoOpTracker;
        assert_eq!(t.get_agent_spawn_count("2024-01-01T00:00:00Z").unwrap(), 0);
    }

    #[test]
    fn test_default_get_cost_estimate_returns_default() {
        let t = NoOpTracker;
        let est = t
            .get_cost_estimate("2024-01-01T00:00:00Z", 100.0, "month")
            .unwrap();
        assert_eq!(est.total_cost, 0.0);
        assert_eq!(est.avg_cost_per_fix, 0.0);
        assert_eq!(est.fix_count, 0);
    }

    #[test]
    fn test_default_get_mttr_trend_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_mttr_trend(4).unwrap().is_empty());
    }

    #[test]
    fn test_default_get_repo_leaderboard_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_repo_leaderboard().unwrap().is_empty());
    }

    #[test]
    fn test_default_get_complexity_time_savings_returns_default() {
        let t = NoOpTracker;
        let savings = t
            .get_complexity_time_savings("2024-01-01T00:00:00Z", 150.0, "month")
            .unwrap();
        assert_eq!(savings.merged_count, 0);
        assert_eq!(savings.hours_saved, 0.0);
        assert_eq!(savings.cost_saved, 0.0);
    }

    #[test]
    fn test_default_get_regression_checks_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_regression_checks(1).unwrap().is_empty());
    }

    #[test]
    fn test_default_list_indexed_repos_returns_empty() {
        let t = NoOpTracker;
        assert!(t.list_indexed_repos().unwrap().is_empty());
    }

    #[test]
    fn test_default_get_indexing_progress_returns_idle_defaults() {
        let t = NoOpTracker;
        let progress = t.get_indexing_progress().unwrap();
        assert_eq!(progress.status, "idle");
        assert_eq!(progress.total_repos, 0);
        assert_eq!(progress.indexed_repos, 0);
        assert!(progress.current_repo.is_none());
        assert_eq!(progress.current_repo_files, 0);
        assert_eq!(progress.total_files_indexed, 0);
        assert!(progress.started_at.is_none());
        assert!(progress.updated_at.is_none());
    }

    #[test]
    fn test_default_subscribe_indexing_progress_returns_valid_receiver() {
        let t = NoOpTracker;
        let rx = t.subscribe_indexing_progress();
        let val = rx.borrow();
        assert_eq!(val.status, "idle");
        assert_eq!(val.total_repos, 0);
        assert_eq!(val.indexed_repos, 0);
    }

    #[test]
    fn test_default_list_all_dependencies_returns_empty() {
        let t = NoOpTracker;
        assert!(t.list_all_dependencies().unwrap().is_empty());
    }

    #[test]
    fn test_default_get_inference_history_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_inference_history(50).unwrap().is_empty());
    }

    #[test]
    fn test_default_list_prs_returns_empty() {
        let t = NoOpTracker;
        assert!(t.list_prs(None, 50).unwrap().is_empty());
        assert!(t.list_prs(Some("open"), 10).unwrap().is_empty());
    }

    #[test]
    fn test_default_store_diff_analysis_returns_zero() {
        let t = NoOpTracker;
        let analysis = crate::types::DiffAnalysis {
            id: 0,
            attempt_id: 1,
            pr_url: "https://github.com/org/repo/pull/1".into(),
            scm_repo: "org/repo".into(),
            pr_number: 1,
            files_changed: vec!["src/main.rs".into()],
            file_types: std::collections::HashMap::new(),
            change_categories: vec![],
            diff_summary: "Fixed a bug".into(),
            created_at: Utc::now(),
        };
        assert_eq!(t.store_diff_analysis(&analysis).unwrap(), 0);
    }

    #[test]
    fn test_default_upsert_promoted_instruction_returns_zero() {
        let t = NoOpTracker;
        let instruction = crate::types::PromotedInstruction {
            id: 0,
            repo: "org/repo".into(),
            source_type: "qa".into(),
            instruction_text: "Always add tests".into(),
            occurrence_count: 5,
            confidence: 0.9,
            is_active: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(t.upsert_promoted_instruction(&instruction).unwrap(), 0);
    }

    #[test]
    fn test_default_upsert_repo_knowledge_returns_zero() {
        let t = NoOpTracker;
        let entry = crate::types::RepoKnowledge {
            id: 0,
            repo: "org/repo".into(),
            knowledge_key: "test_framework".into(),
            knowledge_value: "pytest".into(),
            source_type: "review".into(),
            confidence: 0.8,
            occurrence_count: 3,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(t.upsert_repo_knowledge(&entry).unwrap(), 0);
    }

    #[test]
    fn test_default_upsert_review_pattern_returns_zero() {
        let t = NoOpTracker;
        let pattern = crate::types::ReviewPattern {
            id: 0,
            scm_repo: "org/repo".into(),
            category: crate::types::ReviewCategory::MissingTests,
            pattern_text: "Add unit tests for new functions".into(),
            example_comments: vec!["Please add tests".into()],
            occurrence_count: 4,
            promoted_to_instruction: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert_eq!(t.upsert_review_pattern(&pattern).unwrap(), 0);
    }

    #[test]
    fn test_default_store_strategy_fingerprint_returns_zero() {
        let t = NoOpTracker;
        let fp = crate::types::StrategyFingerprint {
            id: 0,
            attempt_id: 1,
            files_explored: vec!["src/lib.rs".into()],
            tests_run: 5,
            tools_used: std::collections::HashMap::new(),
            fix_approach: "direct_edit".into(),
            strategy_summary: "Edited source directly".into(),
            fix_quality_score: None,
            created_at: Utc::now(),
        };
        assert_eq!(t.store_strategy_fingerprint(&fp).unwrap(), 0);
    }

    #[test]
    fn test_default_store_issue_cluster_returns_zero() {
        let t = NoOpTracker;
        let cluster = crate::types::IssueCluster {
            id: 0,
            cluster_key: "TypeError::main".into(),
            source: "sentry".into(),
            issue_ids: vec!["SENTRY-1".into(), "SENTRY-2".into()],
            window_start: Utc::now(),
            window_end: Utc::now(),
            resolved_by_issue_id: None,
            resolved_by_attempt_id: None,
            status: "active".into(),
            created_at: Utc::now(),
        };
        assert_eq!(t.store_issue_cluster(&cluster).unwrap(), 0);
    }

    #[test]
    fn test_default_store_content_cluster_returns_zero() {
        let t = NoOpTracker;
        let cluster = crate::types::ContentCluster {
            id: 0,
            cluster_key: "TypeError::app.main".into(),
            source: "sentry".into(),
            representative_issue_id: "SENTRY-1".into(),
            issue_ids: vec!["SENTRY-1".into(), "SENTRY-2".into()],
            error_type: Some("TypeError".into()),
            culprit: Some("app.main".into()),
            avg_similarity: 0.85,
            status: "active".into(),
            created_at: Utc::now(),
        };
        assert_eq!(t.store_content_cluster(&cluster).unwrap(), 0);
    }

    #[test]
    fn test_default_get_active_content_clusters_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_active_content_clusters("sentry").unwrap().is_empty());
    }

    #[test]
    fn test_default_resolve_content_cluster_succeeds() {
        let t = NoOpTracker;
        assert!(t.resolve_content_cluster(1).is_ok());
    }

    #[test]
    fn test_default_store_severity_score_succeeds() {
        let t = NoOpTracker;
        let score = crate::types::SeverityScore::default();
        let blast = crate::types::BlastRadius::default();
        assert!(t
            .store_severity_score("sentry", "SENTRY-1", &score, blast)
            .is_ok());
    }

    #[test]
    fn test_default_record_suppression_succeeds() {
        let t = NoOpTracker;
        assert!(t
            .record_suppression("sentry", "SENTRY-1", "flaky_test_rule", "Flaky test noise")
            .is_ok());
    }

    #[test]
    fn test_default_get_recent_attempts_since_returns_empty() {
        let t = NoOpTracker;
        let cutoff = Utc::now();
        assert!(t.get_recent_attempts_since(&cutoff).unwrap().is_empty());
    }

    #[test]
    fn test_default_has_dependency_returns_false() {
        let t = NoOpTracker;
        assert!(!t.has_dependency("repo_a", "repo_b").unwrap());
    }

    #[test]
    fn test_default_upsert_cross_repo_correlation_returns_valid_struct() {
        let t = NoOpTracker;
        let corr = t
            .upsert_cross_repo_correlation("org/repo-a", "org/repo-b", 24)
            .unwrap();
        assert_eq!(corr.id, 0);
        assert_eq!(corr.repo_a, "org/repo-a");
        assert_eq!(corr.repo_b, "org/repo-b");
        assert_eq!(corr.correlation_count, 0);
        assert_eq!(corr.window_hours, 24);
    }

    #[test]
    fn test_default_get_cross_repo_correlations_returns_empty() {
        let t = NoOpTracker;
        assert!(t.get_cross_repo_correlations(2, 168).unwrap().is_empty());
    }
}

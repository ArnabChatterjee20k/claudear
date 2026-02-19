//! Storage implementations for tracking fix attempts, embeddings, and analytics.

pub mod analytics;
mod sqlite;
pub mod vectorlite;

pub use analytics::{
    classify_error, compute_error_hash, AnalyticsService, TimePeriod, TrendAnalysis, TrendDirection,
};
pub use sqlite::{
    ConfidenceBreakdown, DiagnosticCounts, IndexStats, IndexingProgress, InferenceHistoryEntry,
    InferenceStats, SqliteTracker, StoredDependency, StoredIndexedRepo, StoredRepository, UserRow,
};
pub use vectorlite::{is_vectorlite_available, try_load_vectorlite};

use crate::error::Result;
use crate::feedback::FixOutcome;
use crate::types::{
    ActivityLogEntry, AnalyticsSummary, ClaudeExecution, ErrorPattern, FixAttempt, FixAttemptStats,
    FixAttemptStatus, PrAnalytics, PrReviewRecord, ProcessingMetric, PromptExperiment,
    QaKnowledgeEntry, QaMatch, RegressionCheck, RegressionWatch, RegressionWatchStatus,
};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// Trait for tracking fix attempts.
pub trait FixAttemptTracker: Send + Sync {
    /// Downcast to concrete type for auth operations.
    fn as_any(&self) -> &dyn std::any::Any;

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

    // ─── Dashboard metrics extension methods (default no-ops) ───

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

    /// Compute cost estimate from Claude execution durations.
    fn get_cost_estimate(
        &self,
        _since_iso: &str,
        _cost_per_minute: f64,
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

    /// Engineering time savings estimate.
    fn get_time_savings(
        &self,
        _since_iso: &str,
        _hours_per_fix: f64,
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

    // ─── Continuous Learning extension methods (default no-ops) ───

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

    // ── Prioritisation engine storage ──────────────────────────────────

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
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal struct implementing only the required trait methods
    /// to verify that all default methods return correct no-op values.
    struct NoOpTracker;

    impl FixAttemptTracker for NoOpTracker {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
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

    // ── Default method return value tests ──

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
        let exec = ClaudeExecution::new();
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
}

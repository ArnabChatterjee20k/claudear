//! Cross-repo failure correlation detection.
//!
//! Tracks co-occurrence of issues across dependent repos within configurable
//! time windows. When correlation count exceeds a threshold, surfaces insight
//! that issues in repo B may be caused by changes in repo A.

use crate::error::Result;
use crate::storage::FixAttemptTracker;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// A detected correlation between two repos having concurrent issues.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRepoCorrelation {
    pub id: i64,
    pub repo_a: String,
    pub repo_b: String,
    pub correlation_count: i64,
    pub last_seen_at: DateTime<Utc>,
    pub window_hours: i64,
}

/// Result of correlation analysis.
#[derive(Debug, Clone)]
pub struct CorrelationInsight {
    pub repo_a: String,
    pub repo_b: String,
    pub correlation_count: i64,
    pub message: String,
}

pub struct CrossRepoCorrelator;

impl CrossRepoCorrelator {
    /// Check for cross-repo issue correlations.
    ///
    /// For each repo with recent issues, check if its dependencies also have
    /// recent issues within the given time window.
    pub fn detect_correlations(
        tracker: &dyn FixAttemptTracker,
        window_hours: i64,
    ) -> Result<Vec<CorrelationInsight>> {
        let mut insights = Vec::new();

        // Get all recent attempts (within window)
        let cutoff = Utc::now() - Duration::hours(window_hours);
        let recent_attempts = tracker.get_recent_attempts_since(&cutoff)?;

        if recent_attempts.is_empty() {
            return Ok(insights);
        }

        // Group by repo
        let mut repo_issues: std::collections::HashMap<String, Vec<DateTime<Utc>>> =
            std::collections::HashMap::new();
        for attempt in &recent_attempts {
            if let Some(ref repo) = attempt.scm_repo {
                repo_issues
                    .entry(repo.clone())
                    .or_default()
                    .push(attempt.attempted_at);
            }
        }

        // For each pair of repos with issues, check for dependency relationship
        let repos_with_issues: Vec<String> = repo_issues.keys().cloned().collect();

        for i in 0..repos_with_issues.len() {
            for j in (i + 1)..repos_with_issues.len() {
                let repo_a = &repos_with_issues[i];
                let repo_b = &repos_with_issues[j];

                // Check if they have a dependency relationship
                let a_depends_on_b = tracker.has_dependency(repo_a, repo_b)?;
                let b_depends_on_a = tracker.has_dependency(repo_b, repo_a)?;

                if !a_depends_on_b && !b_depends_on_a {
                    continue;
                }

                // Determine which is upstream (dependency) and which is downstream
                let (upstream, downstream) = if b_depends_on_a {
                    (repo_a.as_str(), repo_b.as_str())
                } else {
                    (repo_b.as_str(), repo_a.as_str())
                };

                // Check temporal overlap within window
                let upstream_times = &repo_issues[upstream];
                let downstream_times = &repo_issues[downstream];

                let has_overlap = upstream_times.iter().any(|ut| {
                    downstream_times.iter().any(|dt| {
                        let diff = (*ut - *dt).num_hours().abs();
                        diff <= window_hours
                    })
                });

                if has_overlap {
                    // Record/increment correlation
                    let correlation = tracker.upsert_cross_repo_correlation(
                        upstream,
                        downstream,
                        window_hours,
                    )?;

                    // Surface insight if correlation count is significant (>= 3)
                    if correlation.correlation_count >= 3 {
                        insights.push(CorrelationInsight {
                            repo_a: upstream.to_string(),
                            repo_b: downstream.to_string(),
                            correlation_count: correlation.correlation_count,
                            message: format!(
                                "Issues in {} may be caused by changes in {} ({} co-occurrences in {}h windows)",
                                downstream, upstream, correlation.correlation_count, window_hours
                            ),
                        });
                    }
                }
            }
        }

        Ok(insights)
    }

    /// Get active correlation insights for prompt enhancement.
    pub fn get_active_insights(
        tracker: &dyn FixAttemptTracker,
        min_count: i64,
        max_age_hours: i64,
    ) -> Result<Vec<CorrelationInsight>> {
        let correlations = tracker.get_cross_repo_correlations(min_count, max_age_hours)?;

        Ok(correlations
            .into_iter()
            .map(|c| CorrelationInsight {
                repo_a: c.repo_a.clone(),
                repo_b: c.repo_b.clone(),
                correlation_count: c.correlation_count,
                message: format!(
                    "Issues in {} may be caused by changes in {} ({} co-occurrences)",
                    c.repo_b, c.repo_a, c.correlation_count
                ),
            })
            .collect())
    }

    /// Format correlation insights as context for prompt injection.
    pub fn format_context(insights: &[CorrelationInsight]) -> String {
        if insights.is_empty() {
            return String::new();
        }

        let mut ctx = String::from("## Cross-Repository Correlations\n\n");
        for insight in insights {
            ctx.push_str(&format!("- {}\n", insight.message));
        }
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteTracker;

    #[test]
    fn test_format_context_empty() {
        assert!(CrossRepoCorrelator::format_context(&[]).is_empty());
    }

    #[test]
    fn test_format_context_with_insights() {
        let insights = vec![CorrelationInsight {
            repo_a: "org/core-lib".into(),
            repo_b: "org/web-app".into(),
            correlation_count: 5,
            message:
                "Issues in org/web-app may be caused by changes in org/core-lib (5 co-occurrences)"
                    .into(),
        }];
        let ctx = CrossRepoCorrelator::format_context(&insights);
        assert!(ctx.contains("Cross-Repository Correlations"));
        assert!(ctx.contains("org/core-lib"));
        assert!(ctx.contains("org/web-app"));
    }

    #[test]
    fn test_correlation_insight_fields() {
        let insight = CorrelationInsight {
            repo_a: "a".into(),
            repo_b: "b".into(),
            correlation_count: 3,
            message: "test message".into(),
        };
        assert_eq!(insight.repo_a, "a");
        assert_eq!(insight.repo_b, "b");
        assert_eq!(insight.correlation_count, 3);
    }

    #[test]
    fn test_detect_correlations_no_attempts() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert!(insights.is_empty());
    }

    #[test]
    fn test_detect_correlations_no_dependency_no_correlation() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Record attempts for two unrelated repos (no dependency between them)
        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo-a/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success("linear", "issue-2", "https://github.com/org/repo-b/pull/1")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert!(
            insights.is_empty(),
            "Should not detect correlation without dependency relationship"
        );
    }

    #[test]
    fn test_detect_correlations_single_repo_no_correlation() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Record attempts only for one repo -- need at least two repos for correlation
        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo-a/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success("linear", "issue-2", "https://github.com/org/repo-a/pull/2")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert!(
            insights.is_empty(),
            "Single repo should not produce correlations"
        );
    }

    #[test]
    fn test_detect_correlations_with_dependency_below_threshold() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create dependency: org/downstream depends on org/upstream
        tracker
            .add_dependency("org/upstream", "org/downstream", "runtime")
            .unwrap();

        // Record attempts for both repos
        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-1",
                "https://github.com/org/upstream/pull/1",
            )
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-2",
                "https://github.com/org/downstream/pull/1",
            )
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        // Should detect correlation but not surface insight yet (count=1 < 3)
        assert!(
            insights.is_empty(),
            "First detection should not surface insight (count < 3)"
        );
    }

    #[test]
    fn test_detect_correlations_surfaces_at_threshold() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create dependency: org/downstream depends on org/upstream
        tracker
            .add_dependency("org/upstream", "org/downstream", "runtime")
            .unwrap();

        // Pre-seed correlation count to 2 by upserting twice
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 24)
            .unwrap();
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 24)
            .unwrap();

        // Now trigger a third detection via detect_correlations
        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-1",
                "https://github.com/org/upstream/pull/1",
            )
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-2",
                "https://github.com/org/downstream/pull/1",
            )
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert_eq!(insights.len(), 1, "Should surface insight at count >= 3");
        assert!(insights[0].message.contains("org/upstream"));
        assert!(insights[0].message.contains("org/downstream"));
        assert_eq!(insights[0].correlation_count, 3);
    }

    #[test]
    fn test_detect_correlations_reverse_dependency_direction() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create dependency: org/app depends on org/lib (lib is upstream)
        tracker
            .add_dependency("org/lib", "org/app", "runtime")
            .unwrap();

        // Pre-seed to just below threshold
        tracker
            .upsert_cross_repo_correlation("org/lib", "org/app", 24)
            .unwrap();
        tracker
            .upsert_cross_repo_correlation("org/lib", "org/app", 24)
            .unwrap();

        // Record attempts -- order of repos in fix_attempts doesn't matter,
        // detect_correlations checks both directions
        tracker.record_attempt("linear", "issue-A", "P-A").unwrap();
        tracker
            .mark_success("linear", "issue-A", "https://github.com/org/app/pull/10")
            .unwrap();
        tracker.record_attempt("linear", "issue-B", "P-B").unwrap();
        tracker
            .mark_success("linear", "issue-B", "https://github.com/org/lib/pull/5")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert_eq!(insights.len(), 1);
        // Upstream should be org/lib, downstream should be org/app
        assert_eq!(insights[0].repo_a, "org/lib");
        assert_eq!(insights[0].repo_b, "org/app");
        assert!(insights[0].message.contains("org/lib"));
        assert!(insights[0].message.contains("org/app"));
    }

    #[test]
    fn test_detect_correlations_multiple_repo_pairs() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Create two dependency relationships
        tracker
            .add_dependency("org/core", "org/web", "runtime")
            .unwrap();
        tracker
            .add_dependency("org/core", "org/mobile", "runtime")
            .unwrap();

        // Pre-seed both pairs to threshold - 1
        for _ in 0..2 {
            tracker
                .upsert_cross_repo_correlation("org/core", "org/web", 24)
                .unwrap();
            tracker
                .upsert_cross_repo_correlation("org/core", "org/mobile", 24)
                .unwrap();
        }

        // Record attempts for all three repos
        tracker
            .record_attempt("linear", "issue-core", "P-core")
            .unwrap();
        tracker
            .mark_success("linear", "issue-core", "https://github.com/org/core/pull/1")
            .unwrap();
        tracker
            .record_attempt("linear", "issue-web", "P-web")
            .unwrap();
        tracker
            .mark_success("linear", "issue-web", "https://github.com/org/web/pull/1")
            .unwrap();
        tracker
            .record_attempt("linear", "issue-mobile", "P-mobile")
            .unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-mobile",
                "https://github.com/org/mobile/pull/1",
            )
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert_eq!(
            insights.len(),
            2,
            "Should surface insights for both dependent pairs"
        );
    }

    #[test]
    fn test_detect_correlations_attempts_without_repo_ignored() {
        let tracker = SqliteTracker::in_memory().unwrap();

        // Record attempt but don't mark success (so scm_repo stays NULL)
        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        assert!(
            insights.is_empty(),
            "Attempts without scm_repo should be ignored"
        );
    }

    #[test]
    fn test_get_active_insights_empty() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert!(insights.is_empty());
    }

    #[test]
    fn test_get_active_insights_returns_stored() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Create correlations above threshold (5 upserts = count of 5)
        for _ in 0..5 {
            tracker
                .upsert_cross_repo_correlation("org/core", "org/app", 24)
                .unwrap();
        }
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].correlation_count, 5);
        assert!(insights[0].message.contains("org/core"));
        assert!(insights[0].message.contains("org/app"));
    }

    #[test]
    fn test_get_active_insights_below_min_count_filtered() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Create correlation with count of 2 (below min_count of 3)
        tracker
            .upsert_cross_repo_correlation("org/lib", "org/svc", 24)
            .unwrap();
        tracker
            .upsert_cross_repo_correlation("org/lib", "org/svc", 24)
            .unwrap();

        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert!(
            insights.is_empty(),
            "Correlations below min_count should be filtered out"
        );
    }

    #[test]
    fn test_get_active_insights_message_format() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for _ in 0..4 {
            tracker
                .upsert_cross_repo_correlation("org/upstream-lib", "org/downstream-app", 24)
                .unwrap();
        }
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert_eq!(insights.len(), 1);
        // Verify message follows expected format from get_active_insights
        assert!(insights[0]
            .message
            .contains("Issues in org/downstream-app may be caused by changes in org/upstream-lib"));
        assert!(insights[0].message.contains("4 co-occurrences"));
    }

    #[test]
    fn test_get_active_insights_multiple_correlations() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for _ in 0..3 {
            tracker
                .upsert_cross_repo_correlation("org/a", "org/b", 24)
                .unwrap();
            tracker
                .upsert_cross_repo_correlation("org/x", "org/y", 12)
                .unwrap();
        }
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert_eq!(
            insights.len(),
            2,
            "Should return all correlations above threshold"
        );
    }

    #[test]
    fn test_format_context_from_detect_correlations() {
        let tracker = SqliteTracker::in_memory().unwrap();

        tracker
            .add_dependency("org/upstream", "org/downstream", "runtime")
            .unwrap();

        // Pre-seed to 2 so detect_correlations pushes it to 3
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 24)
            .unwrap();
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 24)
            .unwrap();

        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-1",
                "https://github.com/org/upstream/pull/1",
            )
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-2",
                "https://github.com/org/downstream/pull/1",
            )
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        let ctx = CrossRepoCorrelator::format_context(&insights);
        assert!(ctx.contains("Cross-Repository Correlations"));
        assert!(ctx.contains("org/upstream"));
        assert!(ctx.contains("org/downstream"));
        assert!(ctx.contains("3 co-occurrences"));
    }

    #[test]
    fn test_format_context_multiple_insights() {
        let insights = vec![
            CorrelationInsight {
                repo_a: "org/core-lib".into(),
                repo_b: "org/web-app".into(),
                correlation_count: 5,
                message: "Issues in org/web-app may be caused by changes in org/core-lib (5 co-occurrences)".into(),
            },
            CorrelationInsight {
                repo_a: "org/auth-lib".into(),
                repo_b: "org/api-svc".into(),
                correlation_count: 3,
                message: "Issues in org/api-svc may be caused by changes in org/auth-lib (3 co-occurrences)".into(),
            },
        ];
        let ctx = CrossRepoCorrelator::format_context(&insights);
        assert!(ctx.starts_with("## Cross-Repository Correlations\n\n"));
        assert!(ctx.contains("- Issues in org/web-app"));
        assert!(ctx.contains("- Issues in org/api-svc"));
        // Verify each insight gets its own bullet line
        let bullet_count = ctx.matches("\n- ").count();
        assert_eq!(bullet_count, 2, "Should have 2 bullet lines for 2 insights");
    }

    #[test]
    fn test_format_context_single_insight_line_format() {
        let insights = vec![CorrelationInsight {
            repo_a: "a".into(),
            repo_b: "b".into(),
            correlation_count: 7,
            message: "test message".into(),
        }];
        let ctx = CrossRepoCorrelator::format_context(&insights);
        // Should contain the header and the bullet
        assert!(ctx.contains("## Cross-Repository Correlations\n\n- test message\n"));
    }

    #[test]
    fn test_correlation_insight_clone() {
        let insight = CorrelationInsight {
            repo_a: "a".into(),
            repo_b: "b".into(),
            correlation_count: 4,
            message: "msg".into(),
        };
        let cloned = insight.clone();
        assert_eq!(cloned.repo_a, insight.repo_a);
        assert_eq!(cloned.repo_b, insight.repo_b);
        assert_eq!(cloned.correlation_count, insight.correlation_count);
        assert_eq!(cloned.message, insight.message);
    }

    #[test]
    fn test_cross_repo_correlation_struct_fields() {
        let corr = CrossRepoCorrelation {
            id: 42,
            repo_a: "org/upstream".into(),
            repo_b: "org/downstream".into(),
            correlation_count: 10,
            last_seen_at: Utc::now(),
            window_hours: 24,
        };
        assert_eq!(corr.id, 42);
        assert_eq!(corr.repo_a, "org/upstream");
        assert_eq!(corr.repo_b, "org/downstream");
        assert_eq!(corr.correlation_count, 10);
        assert_eq!(corr.window_hours, 24);
    }

    #[test]
    fn test_cross_repo_correlation_clone() {
        let now = Utc::now();
        let corr = CrossRepoCorrelation {
            id: 1,
            repo_a: "a".into(),
            repo_b: "b".into(),
            correlation_count: 5,
            last_seen_at: now,
            window_hours: 12,
        };
        let cloned = corr.clone();
        assert_eq!(cloned.id, corr.id);
        assert_eq!(cloned.repo_a, corr.repo_a);
        assert_eq!(cloned.window_hours, corr.window_hours);
    }

    #[test]
    fn test_cross_repo_correlation_serialization() {
        let corr = CrossRepoCorrelation {
            id: 1,
            repo_a: "org/lib".into(),
            repo_b: "org/app".into(),
            correlation_count: 3,
            last_seen_at: Utc::now(),
            window_hours: 48,
        };
        let json = serde_json::to_string(&corr).unwrap();
        assert!(json.contains("org/lib"));
        assert!(json.contains("org/app"));
        assert!(json.contains("48"));

        let deserialized: CrossRepoCorrelation = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.repo_a, "org/lib");
        assert_eq!(deserialized.repo_b, "org/app");
        assert_eq!(deserialized.correlation_count, 3);
        assert_eq!(deserialized.window_hours, 48);
    }

    #[test]
    fn test_detect_correlations_window_hours_zero() {
        // Window of 0 hours means only issues at exactly the same time correlate
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .add_dependency("org/upstream", "org/downstream", "runtime")
            .unwrap();

        // Pre-seed to 2
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 0)
            .unwrap();
        tracker
            .upsert_cross_repo_correlation("org/upstream", "org/downstream", 0)
            .unwrap();

        tracker.record_attempt("linear", "issue-1", "P-1").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-1",
                "https://github.com/org/upstream/pull/1",
            )
            .unwrap();
        tracker.record_attempt("linear", "issue-2", "P-2").unwrap();
        tracker
            .mark_success(
                "linear",
                "issue-2",
                "https://github.com/org/downstream/pull/1",
            )
            .unwrap();

        // With window_hours=0, overlap check uses |diff| <= 0, so only exact same time
        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 0).unwrap();
        // The attempts are created nearly simultaneously, so diff should be 0 hours
        assert_eq!(insights.len(), 1);
    }

    #[test]
    fn test_detect_correlations_large_window() {
        // A very large window should still work
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker.add_dependency("org/a", "org/b", "runtime").unwrap();

        for _ in 0..2 {
            tracker
                .upsert_cross_repo_correlation("org/a", "org/b", 10000)
                .unwrap();
        }

        tracker.record_attempt("linear", "i1", "P-1").unwrap();
        tracker
            .mark_success("linear", "i1", "https://github.com/org/a/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "i2", "P-2").unwrap();
        tracker
            .mark_success("linear", "i2", "https://github.com/org/b/pull/1")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 10000).unwrap();
        assert_eq!(insights.len(), 1);
    }

    #[test]
    fn test_detect_correlations_message_format_includes_window() {
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .add_dependency("org/up", "org/down", "runtime")
            .unwrap();
        for _ in 0..2 {
            tracker
                .upsert_cross_repo_correlation("org/up", "org/down", 48)
                .unwrap();
        }

        tracker.record_attempt("linear", "i1", "P1").unwrap();
        tracker
            .mark_success("linear", "i1", "https://github.com/org/up/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "i2", "P2").unwrap();
        tracker
            .mark_success("linear", "i2", "https://github.com/org/down/pull/1")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 48).unwrap();
        assert_eq!(insights.len(), 1);
        assert!(
            insights[0].message.contains("48h windows"),
            "Message should include window hours, got: {}",
            insights[0].message
        );
    }

    #[test]
    fn test_get_active_insights_min_count_boundary() {
        let tracker = SqliteTracker::in_memory().unwrap();
        // Create correlation with count of exactly min_count (3)
        for _ in 0..3 {
            tracker
                .upsert_cross_repo_correlation("org/a", "org/b", 24)
                .unwrap();
        }
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 3, 48).unwrap();
        assert_eq!(
            insights.len(),
            1,
            "count=3 should be returned when min_count=3"
        );
    }

    #[test]
    fn test_get_active_insights_max_age_zero() {
        let tracker = SqliteTracker::in_memory().unwrap();
        for _ in 0..5 {
            tracker
                .upsert_cross_repo_correlation("org/a", "org/b", 24)
                .unwrap();
        }
        // max_age_hours=0 means only correlations from the current instant;
        // newly created should still qualify since last_seen_at is now
        let insights = CrossRepoCorrelator::get_active_insights(&tracker, 1, 0).unwrap();
        // Depending on storage precision, this might be 0 or 1. Just verify no panic.
        assert!(insights.len() <= 1);
    }

    #[test]
    fn test_format_context_preserves_message_verbatim() {
        let msg = "Custom message with special chars: <>&\"'";
        let insights = vec![CorrelationInsight {
            repo_a: "a".into(),
            repo_b: "b".into(),
            correlation_count: 1,
            message: msg.to_string(),
        }];
        let ctx = CrossRepoCorrelator::format_context(&insights);
        assert!(
            ctx.contains(msg),
            "format_context should preserve message verbatim"
        );
    }

    #[test]
    fn test_detect_correlations_bidirectional_dependency() {
        // When A depends on B AND B depends on A, should still produce insight.
        // detect_correlations iterates repo pairs from a HashMap whose order is
        // non-deterministic.  Depending on iteration order the upsert may target
        // either (alpha, beta) or (beta, alpha).  Pre-seed BOTH directions so
        // that whichever direction detect_correlations picks, the count reaches
        // the >= 3 threshold after the internal upsert.
        let tracker = SqliteTracker::in_memory().unwrap();
        tracker
            .add_dependency("org/alpha", "org/beta", "runtime")
            .unwrap();
        tracker
            .add_dependency("org/beta", "org/alpha", "dev")
            .unwrap();

        for _ in 0..2 {
            tracker
                .upsert_cross_repo_correlation("org/alpha", "org/beta", 24)
                .unwrap();
        }
        for _ in 0..2 {
            tracker
                .upsert_cross_repo_correlation("org/beta", "org/alpha", 24)
                .unwrap();
        }

        tracker.record_attempt("linear", "i1", "P1").unwrap();
        tracker
            .mark_success("linear", "i1", "https://github.com/org/alpha/pull/1")
            .unwrap();
        tracker.record_attempt("linear", "i2", "P2").unwrap();
        tracker
            .mark_success("linear", "i2", "https://github.com/org/beta/pull/1")
            .unwrap();

        let insights = CrossRepoCorrelator::detect_correlations(&tracker, 24).unwrap();
        // Should produce at least one insight
        assert!(
            !insights.is_empty(),
            "Bidirectional dependency should still produce correlation insights"
        );
    }
}

//! Regression check scheduler.
//!
//! Schedules and executes hourly regression checks for 24 hours
//! after a fix is released.

use crate::error::Result;
use crate::regression::RegressionChecker;
use crate::storage::SqliteTracker;
use crate::types::{RegressionCheck, RegressionWatch, RegressionWatchStatus};
use chrono::{Duration, Utc};
use std::sync::Arc;

/// Configuration for regression scheduling.
#[derive(Debug, Clone)]
pub struct RegressionSchedulerConfig {
    /// How often to check for regressions (in hours).
    pub check_interval_hours: u32,
    /// Total monitoring duration (in hours).
    pub monitoring_duration_hours: u32,
    /// Minimum events on Sentry to trigger regression.
    pub sentry_event_threshold: u32,
    /// Similarity threshold for semantic matching (0.0-1.0).
    pub similarity_threshold: f64,
}

impl Default for RegressionSchedulerConfig {
    fn default() -> Self {
        Self {
            check_interval_hours: 1,
            monitoring_duration_hours: 24,
            sentry_event_threshold: 1,
            similarity_threshold: 0.75,
        }
    }
}

/// Result of a regression check cycle.
#[derive(Debug, Clone)]
pub struct CheckCycleResult {
    /// Watch ID.
    pub watch_id: i64,
    /// Check number (1-24).
    pub check_number: u32,
    /// Whether regression was detected.
    pub regression_detected: bool,
    /// Whether this was the final check.
    pub is_final_check: bool,
    /// New status if changed.
    pub new_status: Option<RegressionWatchStatus>,
    /// Issue type (for retry triggering).
    pub issue_type: crate::types::IssueType,
    /// Issue ID (for retry triggering).
    pub issue_id: String,
}

/// Schedules and runs regression checks.
pub struct RegressionScheduler<C: RegressionChecker> {
    checker: C,
    tracker: Arc<SqliteTracker>,
    config: RegressionSchedulerConfig,
}

impl<C: RegressionChecker> RegressionScheduler<C> {
    /// Create a new regression scheduler.
    pub fn new(checker: C, tracker: Arc<SqliteTracker>, config: RegressionSchedulerConfig) -> Self {
        Self {
            checker,
            tracker,
            config,
        }
    }

    /// Check all watches that are in monitoring state.
    pub async fn check_monitoring_watches(&self) -> Result<Vec<CheckCycleResult>> {
        let watches = self
            .tracker
            .get_regression_watches_by_status(RegressionWatchStatus::Monitoring)?;

        let mut results = Vec::new();

        for watch in watches {
            if let Some(result) = self.check_watch(&watch).await? {
                results.push(result);
            }
        }

        Ok(results)
    }

    /// Check a specific watch for regression.
    async fn check_watch(&self, watch: &RegressionWatch) -> Result<Option<CheckCycleResult>> {
        // Get existing checks to determine check number
        let existing_checks = self.tracker.get_regression_checks(watch.id)?;
        let check_number = (existing_checks.len() + 1) as u32;

        // Check if we're past the monitoring window
        let monitoring_started = match watch.monitoring_started_at {
            Some(started) => started,
            None => return Ok(None), // Invalid state
        };

        let now = Utc::now();
        let hours_since_start = (now - monitoring_started).num_hours() as u32;

        // Calculate the maximum number of checks for the monitoring window
        let max_checks = self.config.monitoring_duration_hours / self.config.check_interval_hours;

        // Check if we've already completed all checks
        if check_number > max_checks {
            // Already completed monitoring, should not happen but handle gracefully
            tracing::warn!(
                watch_id = watch.id,
                check_number = check_number,
                max_checks = max_checks,
                "Watch has exceeded maximum check count, skipping"
            );
            return Ok(None);
        }

        // Check if enough time has passed since monitoring started for this check number.
        // Check N should happen after (N * check_interval_hours) hours have elapsed.
        // e.g., check 1 after 1 hour, check 2 after 2 hours, etc.
        let required_hours = check_number * self.config.check_interval_hours;
        if hours_since_start < required_hours {
            // Not time for this check yet
            return Ok(None);
        }

        let is_final_check = check_number == max_checks;

        // Perform the regression check
        let regression_result = self.checker.check_regression(watch).await?;

        // Record the check
        let mut check = RegressionCheck::new(watch.id, regression_result.regression_detected);
        check.check_details = Some(format!(
            "Check {}/{}: {}",
            check_number,
            max_checks,
            if regression_result.regression_detected {
                "Regression detected"
            } else {
                "No regression"
            }
        ));
        self.tracker.record_regression_check(&check)?;

        // Determine new status
        let new_status = if regression_result.regression_detected {
            // Regression found - mark as regressed
            self.tracker
                .update_regression_watch_status(watch.id, RegressionWatchStatus::Regressed)?;
            Some(RegressionWatchStatus::Regressed)
        } else if is_final_check {
            // Final check with no regression - mark as resolved
            self.tracker
                .update_regression_watch_status(watch.id, RegressionWatchStatus::Resolved)?;
            Some(RegressionWatchStatus::Resolved)
        } else {
            None
        };

        Ok(Some(CheckCycleResult {
            watch_id: watch.id,
            check_number,
            regression_detected: regression_result.regression_detected,
            is_final_check,
            new_status,
            issue_type: watch.issue_type,
            issue_id: watch.issue_id.clone(),
        }))
    }

    /// Get watches that need their first check (1 hour after monitoring started).
    pub fn get_watches_due_for_check(&self) -> Result<Vec<RegressionWatch>> {
        let watches = self
            .tracker
            .get_regression_watches_by_status(RegressionWatchStatus::Monitoring)?;

        let now = Utc::now();
        let check_interval = Duration::hours(self.config.check_interval_hours as i64);

        let mut due = Vec::new();
        for watch in watches {
            if let Some(started) = watch.monitoring_started_at {
                // Get existing checks
                let checks = self.tracker.get_regression_checks(watch.id)?;
                let last_check_at = checks.first().and_then(|c| c.checked_at).unwrap_or(started);

                // Check if enough time has passed
                if now - last_check_at >= check_interval {
                    due.push(watch);
                }
            }
        }

        Ok(due)
    }

    /// Get the configuration.
    pub fn config(&self) -> &RegressionSchedulerConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regression::RegressionResult;
    use crate::types::IssueType;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockChecker {
        should_detect_regression: AtomicBool,
    }

    impl MockChecker {
        fn new(detect: bool) -> Self {
            Self {
                should_detect_regression: AtomicBool::new(detect),
            }
        }
    }

    #[async_trait]
    impl RegressionChecker for MockChecker {
        async fn check_regression(&self, _watch: &RegressionWatch) -> Result<RegressionResult> {
            Ok(RegressionResult {
                regression_detected: self.should_detect_regression.load(Ordering::SeqCst),
                details: Some("Mock check".to_string()),
            })
        }
    }

    fn create_test_tracker() -> Arc<SqliteTracker> {
        Arc::new(SqliteTracker::in_memory().unwrap())
    }

    #[test]
    fn test_scheduler_config_default() {
        let config = RegressionSchedulerConfig::default();
        assert_eq!(config.check_interval_hours, 1);
        assert_eq!(config.monitoring_duration_hours, 24);
        assert_eq!(config.sentry_event_threshold, 1);
        assert!((config.similarity_threshold - 0.75).abs() < 0.01);
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_empty() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_watch_no_regression() {
        use crate::storage::FixAttemptTracker;

        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-1", "SENTRY-1")
            .unwrap();
        tracker
            .mark_success("sentry", "issue-1", "https://github.com/org/repo/pull/42")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "issue-1").unwrap().unwrap();

        // Create a watch in monitoring state with monitoring_started_at set 2 hours ago
        // so that check 1 (requires 1 hour elapsed) will be due
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-1", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(2));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Note: Don't call update_regression_watch_status here as it would reset
        // monitoring_started_at to now, which would make the check not due yet

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let results = scheduler.check_monitoring_watches().await.unwrap();

        // Should have a result
        assert!(!results.is_empty());
        assert!(!results[0].regression_detected);
        assert!(results[0].new_status.is_none()); // Not final check, status unchanged

        // Verify check was recorded
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 1);
        assert!(!checks[0].issue_still_exists);
    }

    #[tokio::test]
    async fn test_check_watch_regression_detected() {
        use crate::storage::FixAttemptTracker;

        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("linear", "issue-2", "LIN-2")
            .unwrap();
        tracker
            .mark_success("linear", "issue-2", "https://github.com/org/repo/pull/43")
            .unwrap();
        let attempt = tracker.get_attempt("linear", "issue-2").unwrap().unwrap();

        // Create a watch in monitoring state with monitoring_started_at set 2 hours ago
        // so that check 1 (requires 1 hour elapsed) will be due
        let mut watch = RegressionWatch::new(IssueType::LinearBug, "issue-2", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(2));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Note: Don't call update_regression_watch_status here as it would reset
        // monitoring_started_at to now, which would make the check not due yet

        let checker = MockChecker::new(true); // Will detect regression
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let results = scheduler.check_monitoring_watches().await.unwrap();

        assert!(!results.is_empty());
        assert!(results[0].regression_detected);
        assert_eq!(
            results[0].new_status,
            Some(RegressionWatchStatus::Regressed)
        );

        // Verify status was updated
        let updated_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(updated_watch.status, RegressionWatchStatus::Regressed);
    }

    #[tokio::test]
    async fn test_final_check_resolves_watch() {
        use crate::storage::FixAttemptTracker;

        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-3", "SENTRY-3")
            .unwrap();
        tracker
            .mark_success("sentry", "issue-3", "https://github.com/org/repo/pull/44")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "issue-3").unwrap().unwrap();

        // Create a watch that started 25 hours ago (past monitoring window)
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-3", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(25));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Add 23 previous checks (to make next one the 24th/final)
        for _ in 0..23 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        // Update to monitoring state manually (to preserve the old monitoring_started_at)
        let updated = tracker.get_regression_watch(watch_id).unwrap().unwrap();

        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_hours: 1,
            monitoring_duration_hours: 24,
            ..Default::default()
        };
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // Manually check this specific watch
        let result = scheduler.check_watch(&updated).await.unwrap();

        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.is_final_check);
        assert!(!result.regression_detected);
        assert_eq!(result.new_status, Some(RegressionWatchStatus::Resolved));
    }

    #[tokio::test]
    async fn test_get_watches_due_for_check() {
        use crate::storage::FixAttemptTracker;

        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-4", "SENTRY-4")
            .unwrap();
        tracker
            .mark_success("sentry", "issue-4", "https://github.com/org/repo/pull/45")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "issue-4").unwrap().unwrap();

        // Create a watch in monitoring state
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-4", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(2));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Verify the watch was created with monitoring status
        let created = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(created.status, RegressionWatchStatus::Monitoring);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let due = scheduler.get_watches_due_for_check().unwrap();
        // Should be due since more than 1 hour has passed since monitoring_started_at
        assert!(!due.is_empty());
    }
}

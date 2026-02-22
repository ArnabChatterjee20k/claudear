//! Regression check scheduler.
//!
//! Schedules and executes hourly regression checks for 24 hours
//! after a fix is released.

use crate::error::Result;
use crate::regression::RegressionChecker;
use crate::storage::FixAttemptTracker;
use crate::types::{RegressionCheck, RegressionWatch, RegressionWatchStatus};
use chrono::{Duration, Utc};
use std::sync::Arc;

/// Configuration for regression scheduling.
#[derive(Debug, Clone)]
pub struct RegressionSchedulerConfig {
    /// How often to check for regressions (in seconds).
    pub check_interval_secs: u64,
    /// Total monitoring duration (in seconds).
    pub monitoring_duration_secs: u64,
    /// Minimum events on Sentry to trigger regression.
    pub sentry_event_threshold: u32,
    /// Similarity threshold for semantic matching (0.0-1.0).
    pub similarity_threshold: f64,
}

impl Default for RegressionSchedulerConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 3600,       // 1 hour
            monitoring_duration_secs: 86400, // 24 hours
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
    tracker: Arc<dyn FixAttemptTracker>,
    config: RegressionSchedulerConfig,
}

impl<C: RegressionChecker> RegressionScheduler<C> {
    /// Create a new regression scheduler.
    pub fn new(
        checker: C,
        tracker: Arc<dyn FixAttemptTracker>,
        config: RegressionSchedulerConfig,
    ) -> Self {
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
        let secs_since_start = (now - monitoring_started).num_seconds().max(0) as u64;

        // Calculate the maximum number of checks for the monitoring window
        let max_checks =
            self.config.monitoring_duration_secs / self.config.check_interval_secs.max(1);

        // Check if we've already completed all checks
        if check_number as u64 > max_checks {
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
        // Check N should happen after (N * check_interval_secs) seconds have elapsed.
        // e.g., with 10s interval: check 1 after 10s, check 2 after 20s, etc.
        let required_secs = check_number as u64 * self.config.check_interval_secs;
        if secs_since_start < required_secs {
            // Not time for this check yet
            return Ok(None);
        }

        let is_final_check = check_number as u64 == max_checks;

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
        let check_interval = Duration::seconds(self.config.check_interval_secs as i64);

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

    fn create_test_tracker() -> Arc<dyn FixAttemptTracker> {
        Arc::new(crate::storage::SqliteTracker::in_memory().unwrap())
    }

    #[test]
    fn test_scheduler_config_default() {
        let config = RegressionSchedulerConfig::default();
        assert_eq!(config.check_interval_secs, 3600);
        assert_eq!(config.monitoring_duration_secs, 86400);
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
            check_interval_secs: 3600,
            monitoring_duration_secs: 86400,
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

    #[tokio::test]
    async fn test_check_watch_no_monitoring_started_at() {
        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-no-start", "SENTRY-NS")
            .unwrap();
        tracker
            .mark_success(
                "sentry",
                "issue-no-start",
                "https://github.com/org/repo/pull/50",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "issue-no-start")
            .unwrap()
            .unwrap();

        // Create a watch WITHOUT monitoring_started_at (None)
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-no-start", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        // monitoring_started_at is None by default
        assert!(watch.monitoring_started_at.is_none());
        let watch_id = tracker.create_regression_watch(&watch).unwrap();
        let stored_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        // check_watch should return None because monitoring_started_at is None
        let result = scheduler.check_watch(&stored_watch).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_watch_not_time_yet() {
        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-early", "SENTRY-EARLY")
            .unwrap();
        tracker
            .mark_success(
                "sentry",
                "issue-early",
                "https://github.com/org/repo/pull/51",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "issue-early")
            .unwrap()
            .unwrap();

        // Create a watch that just started monitoring (now), so check 1 is not due yet
        // (check 1 requires check_interval_secs to have elapsed)
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-early", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now()); // Just started
        let watch_id = tracker.create_regression_watch(&watch).unwrap();
        let stored_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(), // 3600s interval
        );

        // Should return None since not enough time has elapsed for check 1
        let result = scheduler.check_watch(&stored_watch).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_watch_exceeded_max_checks() {
        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-maxed", "SENTRY-MAX")
            .unwrap();
        tracker
            .mark_success(
                "sentry",
                "issue-maxed",
                "https://github.com/org/repo/pull/52",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "issue-maxed")
            .unwrap()
            .unwrap();

        // Create a watch with monitoring_started_at far enough in the past
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-maxed", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(30));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Record 25 checks (exceeds max of 24 for default config: 86400/3600 = 24)
        for _ in 0..25 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        // check_number would be 26 which exceeds max_checks of 24, returns None with warning
        let result = scheduler.check_watch(&stored_watch).await.unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_config_accessor() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_secs: 1800,
            monitoring_duration_secs: 43200,
            sentry_event_threshold: 5,
            similarity_threshold: 0.9,
        };
        let scheduler = RegressionScheduler::new(checker, tracker, config);

        let cfg = scheduler.config();
        assert_eq!(cfg.check_interval_secs, 1800);
        assert_eq!(cfg.monitoring_duration_secs, 43200);
        assert_eq!(cfg.sentry_event_threshold, 5);
        assert!((cfg.similarity_threshold - 0.9).abs() < 0.001);
    }

    #[test]
    fn test_scheduler_config_custom_values() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: 600,       // 10 minutes
            monitoring_duration_secs: 7200, // 2 hours
            sentry_event_threshold: 10,
            similarity_threshold: 0.5,
        };

        assert_eq!(config.check_interval_secs, 600);
        assert_eq!(config.monitoring_duration_secs, 7200);
        assert_eq!(config.sentry_event_threshold, 10);
        assert!((config.similarity_threshold - 0.5).abs() < 0.001);

        // Verify max_checks computed from these custom values
        let max_checks = config.monitoring_duration_secs / config.check_interval_secs.max(1);
        assert_eq!(max_checks, 12); // 7200 / 600 = 12 checks
    }

    #[test]
    fn test_check_cycle_result_fields() {
        let result = CheckCycleResult {
            watch_id: 42,
            check_number: 5,
            regression_detected: true,
            is_final_check: false,
            new_status: Some(RegressionWatchStatus::Regressed),
            issue_type: IssueType::LinearBug,
            issue_id: "LIN-999".to_string(),
        };

        assert_eq!(result.watch_id, 42);
        assert_eq!(result.check_number, 5);
        assert!(result.regression_detected);
        assert!(!result.is_final_check);
        assert_eq!(result.new_status, Some(RegressionWatchStatus::Regressed));
        assert_eq!(result.issue_type, IssueType::LinearBug);
        assert_eq!(result.issue_id, "LIN-999");

        // Also verify with no regression and final check
        let result_final = CheckCycleResult {
            watch_id: 7,
            check_number: 24,
            regression_detected: false,
            is_final_check: true,
            new_status: Some(RegressionWatchStatus::Resolved),
            issue_type: IssueType::SentryIssue,
            issue_id: "SENTRY-100".to_string(),
        };

        assert_eq!(result_final.watch_id, 7);
        assert_eq!(result_final.check_number, 24);
        assert!(!result_final.regression_detected);
        assert!(result_final.is_final_check);
        assert_eq!(
            result_final.new_status,
            Some(RegressionWatchStatus::Resolved)
        );
        assert_eq!(result_final.issue_type, IssueType::SentryIssue);
        assert_eq!(result_final.issue_id, "SENTRY-100");
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_multiple() {
        let tracker = create_test_tracker();

        // Create two fix attempts
        tracker
            .record_attempt("sentry", "multi-1", "SENTRY-M1")
            .unwrap();
        tracker
            .mark_success("sentry", "multi-1", "https://github.com/org/repo/pull/60")
            .unwrap();
        let attempt1 = tracker.get_attempt("sentry", "multi-1").unwrap().unwrap();

        tracker
            .record_attempt("linear", "multi-2", "LIN-M2")
            .unwrap();
        tracker
            .mark_success("linear", "multi-2", "https://github.com/org/repo/pull/61")
            .unwrap();
        let attempt2 = tracker.get_attempt("linear", "multi-2").unwrap().unwrap();

        // Create two watches both in monitoring state with started 2 hours ago
        let mut watch1 = RegressionWatch::new(IssueType::SentryIssue, "multi-1", attempt1.id);
        watch1.status = RegressionWatchStatus::Monitoring;
        watch1.monitoring_started_at = Some(Utc::now() - Duration::hours(2));
        tracker.create_regression_watch(&watch1).unwrap();

        let mut watch2 = RegressionWatch::new(IssueType::LinearBug, "multi-2", attempt2.id);
        watch2.status = RegressionWatchStatus::Monitoring;
        watch2.monitoring_started_at = Some(Utc::now() - Duration::hours(2));
        tracker.create_regression_watch(&watch2).unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let results = scheduler.check_monitoring_watches().await.unwrap();

        // Both watches should have been processed
        assert_eq!(results.len(), 2);
        assert!(!results[0].regression_detected);
        assert!(!results[1].regression_detected);

        // Verify both are check_number 1
        assert_eq!(results[0].check_number, 1);
        assert_eq!(results[1].check_number, 1);
    }

    #[tokio::test]
    async fn test_check_number_progresses_correctly() {
        let tracker = create_test_tracker();

        // Create a fix attempt
        tracker
            .record_attempt("sentry", "issue-prog", "SENTRY-PROG")
            .unwrap();
        tracker
            .mark_success(
                "sentry",
                "issue-prog",
                "https://github.com/org/repo/pull/70",
            )
            .unwrap();
        let attempt = tracker
            .get_attempt("sentry", "issue-prog")
            .unwrap()
            .unwrap();

        // Create a watch started 10 hours ago (enough time for multiple checks at 1h interval)
        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "issue-prog", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(10));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();

        // Add 5 prior checks
        for _ in 0..5 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        // With 5 existing checks, the next check_number should be 6
        let result = scheduler.check_watch(&stored_watch).await.unwrap();
        assert!(result.is_some());
        let result = result.unwrap();
        assert_eq!(result.check_number, 6);
        assert!(!result.is_final_check); // 6 < 24
        assert!(!result.regression_detected);

        // Verify a 6th check was recorded (total now 6)
        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 6);
    }

    // ================================================================
    // New comprehensive tests below
    // ================================================================

    // --- MockChecker variant that returns errors ---
    struct FailingChecker;

    #[async_trait]
    impl RegressionChecker for FailingChecker {
        async fn check_regression(&self, _watch: &RegressionWatch) -> Result<RegressionResult> {
            Err(crate::error::Error::api("Sentry API unavailable"))
        }
    }

    // --- MockChecker that toggles behavior per call ---
    struct CountingChecker {
        call_count: std::sync::atomic::AtomicU32,
        /// Regression detected on Nth call (0-indexed). None = never detect.
        detect_on_call: Option<u32>,
    }

    impl CountingChecker {
        fn new(detect_on_call: Option<u32>) -> Self {
            Self {
                call_count: std::sync::atomic::AtomicU32::new(0),
                detect_on_call,
            }
        }
        fn calls(&self) -> u32 {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl RegressionChecker for CountingChecker {
        async fn check_regression(&self, _watch: &RegressionWatch) -> Result<RegressionResult> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            let detected = self.detect_on_call == Some(n);
            Ok(RegressionResult {
                regression_detected: detected,
                details: Some(format!("Call #{}", n)),
            })
        }
    }

    /// Helper: create a fix attempt + monitoring watch with specified start time.
    /// Returns (watch_id, stored_watch).
    fn setup_watch(
        tracker: &Arc<dyn FixAttemptTracker>,
        source: &str,
        issue_id: &str,
        short_id: &str,
        started_hours_ago: i64,
    ) -> (i64, RegressionWatch) {
        tracker.record_attempt(source, issue_id, short_id).unwrap();
        tracker
            .mark_success(
                source,
                issue_id,
                &format!("https://github.com/org/repo/pull/{}", short_id),
            )
            .unwrap();
        let attempt = tracker.get_attempt(source, issue_id).unwrap().unwrap();

        let issue_type = match source {
            "sentry" => IssueType::SentryIssue,
            _ => IssueType::LinearBug,
        };

        let mut watch = RegressionWatch::new(issue_type, issue_id, attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(started_hours_ago));
        let watch_id = tracker.create_regression_watch(&watch).unwrap();
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        (watch_id, stored)
    }

    // ----- RegressionSchedulerConfig tests -----

    #[test]
    fn test_config_clone() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: 500,
            monitoring_duration_secs: 10000,
            sentry_event_threshold: 3,
            similarity_threshold: 0.88,
        };
        let cloned = config.clone();
        assert_eq!(cloned.check_interval_secs, 500);
        assert_eq!(cloned.monitoring_duration_secs, 10000);
        assert_eq!(cloned.sentry_event_threshold, 3);
        assert!((cloned.similarity_threshold - 0.88).abs() < f64::EPSILON);
    }

    #[test]
    fn test_config_debug() {
        let config = RegressionSchedulerConfig::default();
        let debug = format!("{:?}", config);
        assert!(debug.contains("RegressionSchedulerConfig"));
        assert!(debug.contains("3600"));
        assert!(debug.contains("86400"));
    }

    #[test]
    fn test_config_max_checks_calculation_various() {
        // 1-second interval, 10 second window => 10 checks
        let config = RegressionSchedulerConfig {
            check_interval_secs: 1,
            monitoring_duration_secs: 10,
            sentry_event_threshold: 1,
            similarity_threshold: 0.5,
        };
        assert_eq!(
            config.monitoring_duration_secs / config.check_interval_secs.max(1),
            10
        );

        // 30-minute interval, 24h window => 48 checks
        let config2 = RegressionSchedulerConfig {
            check_interval_secs: 1800,
            monitoring_duration_secs: 86400,
            sentry_event_threshold: 1,
            similarity_threshold: 0.75,
        };
        assert_eq!(
            config2.monitoring_duration_secs / config2.check_interval_secs.max(1),
            48
        );
    }

    #[test]
    fn test_config_zero_interval_max_checks() {
        // check_interval_secs = 0 should be guarded by .max(1) in production code
        let config = RegressionSchedulerConfig {
            check_interval_secs: 0,
            monitoring_duration_secs: 100,
            sentry_event_threshold: 1,
            similarity_threshold: 0.5,
        };
        // Production code uses .max(1) to avoid division by zero
        let max_checks = config.monitoring_duration_secs / config.check_interval_secs.max(1);
        assert_eq!(max_checks, 100);
    }

    #[test]
    fn test_config_equal_interval_and_duration() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: 3600,
            monitoring_duration_secs: 3600,
            sentry_event_threshold: 1,
            similarity_threshold: 0.75,
        };
        let max_checks = config.monitoring_duration_secs / config.check_interval_secs.max(1);
        assert_eq!(max_checks, 1);
    }

    #[test]
    fn test_config_interval_larger_than_duration() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: 7200,
            monitoring_duration_secs: 3600,
            sentry_event_threshold: 1,
            similarity_threshold: 0.75,
        };
        let max_checks = config.monitoring_duration_secs / config.check_interval_secs.max(1);
        assert_eq!(max_checks, 0); // No checks possible
    }

    #[test]
    fn test_config_similarity_threshold_boundaries() {
        let config_zero = RegressionSchedulerConfig {
            similarity_threshold: 0.0,
            ..Default::default()
        };
        assert!((config_zero.similarity_threshold - 0.0).abs() < f64::EPSILON);

        let config_one = RegressionSchedulerConfig {
            similarity_threshold: 1.0,
            ..Default::default()
        };
        assert!((config_one.similarity_threshold - 1.0).abs() < f64::EPSILON);
    }

    // ----- CheckCycleResult tests -----

    #[test]
    fn test_check_cycle_result_clone() {
        let result = CheckCycleResult {
            watch_id: 1,
            check_number: 10,
            regression_detected: false,
            is_final_check: false,
            new_status: None,
            issue_type: IssueType::SentryIssue,
            issue_id: "test-clone".to_string(),
        };
        let cloned = result.clone();
        assert_eq!(cloned.watch_id, 1);
        assert_eq!(cloned.check_number, 10);
        assert_eq!(cloned.issue_id, "test-clone");
    }

    #[test]
    fn test_check_cycle_result_debug() {
        let result = CheckCycleResult {
            watch_id: 99,
            check_number: 1,
            regression_detected: true,
            is_final_check: true,
            new_status: Some(RegressionWatchStatus::Regressed),
            issue_type: IssueType::LinearBug,
            issue_id: "debug-test".to_string(),
        };
        let debug = format!("{:?}", result);
        assert!(debug.contains("CheckCycleResult"));
        assert!(debug.contains("99"));
    }

    #[test]
    fn test_check_cycle_result_no_status_change() {
        let result = CheckCycleResult {
            watch_id: 5,
            check_number: 3,
            regression_detected: false,
            is_final_check: false,
            new_status: None,
            issue_type: IssueType::SentryIssue,
            issue_id: "no-change".to_string(),
        };
        assert!(result.new_status.is_none());
        assert!(!result.is_final_check);
        assert!(!result.regression_detected);
    }

    // ----- RegressionScheduler::new tests -----

    #[test]
    fn test_scheduler_new_stores_config() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_secs: 999,
            monitoring_duration_secs: 8888,
            sentry_event_threshold: 7,
            similarity_threshold: 0.42,
        };
        let scheduler = RegressionScheduler::new(checker, tracker, config);
        assert_eq!(scheduler.config().check_interval_secs, 999);
        assert_eq!(scheduler.config().monitoring_duration_secs, 8888);
        assert_eq!(scheduler.config().sentry_event_threshold, 7);
        assert!((scheduler.config().similarity_threshold - 0.42).abs() < f64::EPSILON);
    }

    #[test]
    fn test_scheduler_new_with_default_config() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());
        assert_eq!(scheduler.config().check_interval_secs, 3600);
    }

    // ----- check_watch edge cases -----

    #[tokio::test]
    async fn test_check_watch_with_short_interval() {
        let tracker = create_test_tracker();
        let (_, stored_watch) = setup_watch(&tracker, "sentry", "short-int", "SI-1", 1);

        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10,        // 10 seconds
            monitoring_duration_secs: 3600, // 1 hour => 360 checks
            ..Default::default()
        };
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let result = scheduler.check_watch(&stored_watch).await.unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.check_number, 1);
        assert!(!r.is_final_check);
    }

    #[tokio::test]
    async fn test_check_watch_exactly_at_max_checks_is_final() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "exact-max", "EM-1", 5);

        // Config: 5 checks total (5000 / 1000)
        let config = RegressionSchedulerConfig {
            check_interval_secs: 1000,
            monitoring_duration_secs: 5000,
            ..Default::default()
        };

        // Add 4 prior checks (next will be #5 which is max)
        for _ in 0..4 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.check_number, 5);
        assert!(r.is_final_check);
        assert_eq!(r.new_status, Some(RegressionWatchStatus::Resolved));
    }

    #[tokio::test]
    async fn test_check_watch_one_past_max_returns_none() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "past-max", "PM-1", 10);

        let config = RegressionSchedulerConfig {
            check_interval_secs: 1000,
            monitoring_duration_secs: 3000, // max 3 checks
            ..Default::default()
        };

        // Add exactly 3 checks (max), so next would be #4 > max
        for _ in 0..3 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_watch_regression_on_final_check() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "linear", "reg-final", "RF-1", 5);

        let config = RegressionSchedulerConfig {
            check_interval_secs: 1000,
            monitoring_duration_secs: 3000, // max 3 checks
            ..Default::default()
        };

        // Add 2 prior checks, next is #3 (final)
        for _ in 0..2 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(true); // regression detected
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.is_final_check);
        assert!(r.regression_detected);
        // Regression takes priority: status should be Regressed, not Resolved
        assert_eq!(r.new_status, Some(RegressionWatchStatus::Regressed));
    }

    #[tokio::test]
    async fn test_check_watch_check_details_format_no_regression() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "details-no", "DN-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_watch(&stored).await.unwrap();

        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 1);
        let details = checks[0].check_details.as_ref().unwrap();
        assert!(details.contains("No regression"));
        assert!(details.contains("Check 1/24"));
    }

    #[tokio::test]
    async fn test_check_watch_check_details_format_regression() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "details-yes", "DY-1", 2);

        let checker = MockChecker::new(true);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_watch(&stored).await.unwrap();

        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 1);
        let details = checks[0].check_details.as_ref().unwrap();
        assert!(details.contains("Regression detected"));
        assert!(details.contains("Check 1/24"));
    }

    #[tokio::test]
    async fn test_check_watch_records_issue_still_exists_correctly() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "exists-flag", "EF-1", 2);

        // Regression detected = issue_still_exists = true
        let checker = MockChecker::new(true);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );
        scheduler.check_watch(&stored).await.unwrap();

        let checks = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks.len(), 1);
        assert!(checks[0].issue_still_exists);
    }

    #[tokio::test]
    async fn test_check_watch_returns_correct_issue_type_sentry() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "sentry", "type-sentry", "TS-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.issue_type, IssueType::SentryIssue);
        assert_eq!(result.issue_id, "type-sentry");
    }

    #[tokio::test]
    async fn test_check_watch_returns_correct_issue_type_linear() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "linear", "type-linear", "TL-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.issue_type, IssueType::LinearBug);
        assert_eq!(result.issue_id, "type-linear");
    }

    #[tokio::test]
    async fn test_check_watch_status_persisted_to_db_regressed() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "persist-reg", "PR-1", 2);

        let checker = MockChecker::new(true);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_watch(&stored).await.unwrap();

        let db_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(db_watch.status, RegressionWatchStatus::Regressed);
    }

    #[tokio::test]
    async fn test_check_watch_status_persisted_to_db_resolved() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "persist-res", "PRES-1", 26);

        let config = RegressionSchedulerConfig {
            check_interval_secs: 1000,
            monitoring_duration_secs: 2000, // 2 checks max
            ..Default::default()
        };

        // Add 1 prior check so next is #2 (final)
        let check = RegressionCheck::new(watch_id, false);
        tracker.record_regression_check(&check).unwrap();

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        scheduler.check_watch(&stored).await.unwrap();

        let db_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(db_watch.status, RegressionWatchStatus::Resolved);
    }

    #[tokio::test]
    async fn test_check_watch_no_status_change_mid_monitoring() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "no-change", "NC-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert!(result.new_status.is_none());

        // DB status should remain Monitoring
        let db_watch = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        assert_eq!(db_watch.status, RegressionWatchStatus::Monitoring);
    }

    // ----- check_monitoring_watches tests -----

    #[tokio::test]
    async fn test_check_monitoring_watches_ignores_non_monitoring() {
        let tracker = create_test_tracker();

        // Create a watch in AwaitingRelease status (not Monitoring)
        tracker.record_attempt("sentry", "aw-1", "AW-1").unwrap();
        tracker
            .mark_success("sentry", "aw-1", "https://github.com/org/repo/pull/100")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "aw-1").unwrap().unwrap();

        let watch = RegressionWatch::new(IssueType::SentryIssue, "aw-1", attempt.id);
        // Default status is AwaitingRelease, not Monitoring
        tracker.create_regression_watch(&watch).unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_skips_resolved() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "resolved-1", "R-1", 3);

        // Manually mark as resolved
        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Resolved)
            .unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_skips_regressed() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "regressed-1", "REG-1", 3);

        tracker
            .update_regression_watch_status(watch_id, RegressionWatchStatus::Regressed)
            .unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_mixed_statuses() {
        let tracker = create_test_tracker();

        // One monitoring watch (due)
        let (_, _) = setup_watch(&tracker, "sentry", "mix-mon", "MM-1", 2);

        // One awaiting release
        {
            tracker.record_attempt("sentry", "mix-aw", "MA-1").unwrap();
            tracker
                .mark_success("sentry", "mix-aw", "https://github.com/org/repo/pull/200")
                .unwrap();
            let attempt = tracker.get_attempt("sentry", "mix-aw").unwrap().unwrap();
            let watch = RegressionWatch::new(IssueType::SentryIssue, "mix-aw", attempt.id);
            tracker.create_regression_watch(&watch).unwrap();
        }

        // One resolved
        let (resolved_id, _) = setup_watch(&tracker, "sentry", "mix-res", "MR-1", 5);
        tracker
            .update_regression_watch_status(resolved_id, RegressionWatchStatus::Resolved)
            .unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        // Only the monitoring watch should produce a result
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].issue_id, "mix-mon");
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_three_watches_all_due() {
        let tracker = create_test_tracker();

        setup_watch(&tracker, "sentry", "three-1", "T1", 3);
        setup_watch(&tracker, "linear", "three-2", "T2", 4);
        setup_watch(&tracker, "sentry", "three-3", "T3", 5);

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert_eq!(results.len(), 3);
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_some_not_due() {
        let tracker = create_test_tracker();

        // Due (started 2 hours ago, default interval 1 hour)
        setup_watch(&tracker, "sentry", "due-1", "D1", 2);

        // NOT due (started now, check 1 requires 1 hour)
        setup_watch(&tracker, "sentry", "notdue-1", "ND1", 0);

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        // Only the one started 2 hours ago should be processed
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].issue_id, "due-1");
    }

    // ----- Failing checker tests -----

    #[tokio::test]
    async fn test_check_watch_propagates_checker_error() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "sentry", "fail-1", "F1", 2);

        let checker = FailingChecker;
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_propagates_checker_error() {
        let tracker = create_test_tracker();
        setup_watch(&tracker, "sentry", "fail-mon", "FM1", 2);

        let checker = FailingChecker;
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_monitoring_watches().await;
        assert!(result.is_err());
    }

    // ----- CountingChecker tests -----

    #[tokio::test]
    async fn test_counting_checker_tracks_calls() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "sentry", "count-1", "C1", 2);

        let checker = CountingChecker::new(None);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_watch(&stored).await.unwrap();
        assert_eq!(scheduler.checker.calls(), 1);
    }

    #[tokio::test]
    async fn test_counting_checker_multiple_watches_counts_all() {
        let tracker = create_test_tracker();
        setup_watch(&tracker, "sentry", "cc-1", "CC1", 2);
        setup_watch(&tracker, "sentry", "cc-2", "CC2", 3);
        setup_watch(&tracker, "linear", "cc-3", "CC3", 4);

        let checker = CountingChecker::new(None);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_monitoring_watches().await.unwrap();
        assert_eq!(scheduler.checker.calls(), 3);
    }

    #[tokio::test]
    async fn test_counting_checker_detects_on_specific_call() {
        let tracker = create_test_tracker();
        setup_watch(&tracker, "sentry", "detect-0", "D0", 2);
        setup_watch(&tracker, "sentry", "detect-1", "D1a", 3);

        // Detect on second call (0-indexed)
        let checker = CountingChecker::new(Some(1));
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let results = scheduler.check_monitoring_watches().await.unwrap();
        assert_eq!(results.len(), 2);
        // First call: no regression
        assert!(!results[0].regression_detected);
        // Second call: regression detected
        assert!(results[1].regression_detected);
    }

    // ----- get_watches_due_for_check tests -----

    #[test]
    fn test_get_watches_due_for_check_empty_db() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());
        let due = scheduler.get_watches_due_for_check().unwrap();
        assert!(due.is_empty());
    }

    #[test]
    fn test_get_watches_due_for_check_not_due_yet() {
        let tracker = create_test_tracker();
        // Started 30 minutes ago; default interval is 1 hour
        setup_watch(&tracker, "sentry", "notdue", "ND-2", 0);

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let due = scheduler.get_watches_due_for_check().unwrap();
        // monitoring_started_at is "now", and we need at least check_interval_secs (3600s)
        // to pass since last_check_at (which defaults to started if no checks).
        // Since 0 hours < 1 hour, this should NOT be due.
        assert!(due.is_empty());
    }

    #[test]
    fn test_get_watches_due_for_check_exactly_due() {
        let tracker = create_test_tracker();
        // Started exactly 1 hour ago
        setup_watch(&tracker, "sentry", "exact-due", "ED-1", 1);

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let due = scheduler.get_watches_due_for_check().unwrap();
        assert_eq!(due.len(), 1);
    }

    #[test]
    fn test_get_watches_due_for_check_multiple_due() {
        let tracker = create_test_tracker();
        setup_watch(&tracker, "sentry", "multi-due-1", "MD1", 2);
        setup_watch(&tracker, "linear", "multi-due-2", "MD2", 3);

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let due = scheduler.get_watches_due_for_check().unwrap();
        assert_eq!(due.len(), 2);
    }

    #[test]
    fn test_get_watches_due_for_check_no_monitoring_started_at_excluded() {
        let tracker = create_test_tracker();
        tracker
            .record_attempt("sentry", "no-start", "NS-2")
            .unwrap();
        tracker
            .mark_success("sentry", "no-start", "https://github.com/org/repo/pull/300")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "no-start").unwrap().unwrap();

        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "no-start", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        // monitoring_started_at is None
        tracker.create_regression_watch(&watch).unwrap();

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let due = scheduler.get_watches_due_for_check().unwrap();
        // Should be excluded because monitoring_started_at is None
        assert!(due.is_empty());
    }

    #[test]
    fn test_get_watches_due_for_check_with_recent_check() {
        let tracker = create_test_tracker();
        let (watch_id, _) = setup_watch(&tracker, "sentry", "recent-check", "RC-1", 5);

        // Record a check that happened "now" (most recent check)
        let check = RegressionCheck::new(watch_id, false);
        tracker.record_regression_check(&check).unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let due = scheduler.get_watches_due_for_check().unwrap();
        // The last check was "now", so not enough time has passed for the next check
        assert!(due.is_empty());
    }

    #[test]
    fn test_get_watches_due_for_check_custom_interval() {
        let tracker = create_test_tracker();
        // Started 30 seconds ago
        {
            tracker
                .record_attempt("sentry", "cust-int", "CI-1")
                .unwrap();
            tracker
                .mark_success("sentry", "cust-int", "https://github.com/org/repo/pull/400")
                .unwrap();
            let attempt = tracker.get_attempt("sentry", "cust-int").unwrap().unwrap();

            let mut watch = RegressionWatch::new(IssueType::SentryIssue, "cust-int", attempt.id);
            watch.status = RegressionWatchStatus::Monitoring;
            watch.monitoring_started_at = Some(Utc::now() - Duration::seconds(30));
            tracker.create_regression_watch(&watch).unwrap();
        }

        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10, // 10 seconds
            monitoring_duration_secs: 600,
            ..Default::default()
        };
        let scheduler = RegressionScheduler::new(checker, tracker, config);

        let due = scheduler.get_watches_due_for_check().unwrap();
        // 30 seconds > 10 seconds interval, so it should be due
        assert_eq!(due.len(), 1);
    }

    // ----- Config accessor -----

    #[test]
    fn test_config_accessor_returns_reference() {
        let tracker = create_test_tracker();
        let checker = MockChecker::new(false);
        let config = RegressionSchedulerConfig {
            check_interval_secs: 42,
            monitoring_duration_secs: 84,
            sentry_event_threshold: 2,
            similarity_threshold: 0.33,
        };
        let scheduler = RegressionScheduler::new(checker, tracker, config);

        // Calling config() multiple times should return the same values
        let c1 = scheduler.config();
        let c2 = scheduler.config();
        assert_eq!(c1.check_interval_secs, c2.check_interval_secs);
        assert_eq!(c1.monitoring_duration_secs, c2.monitoring_duration_secs);
    }

    // ----- Complex scenario tests -----

    #[tokio::test]
    async fn test_scenario_full_monitoring_lifecycle_no_regression() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10,
            monitoring_duration_secs: 30, // 3 checks max
            ..Default::default()
        };

        // Start 31+ seconds ago to ensure all checks are due
        let (watch_id, _) = setup_watch(&tracker, "sentry", "lifecycle-1", "LC-1", 1);

        let checker = MockChecker::new(false); // No regression ever
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // Check 1
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let r1 = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(r1.check_number, 1);
        assert!(!r1.is_final_check);
        assert!(r1.new_status.is_none());

        // Check 2
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let r2 = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(r2.check_number, 2);
        assert!(!r2.is_final_check);
        assert!(r2.new_status.is_none());

        // Check 3 (final)
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let r3 = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(r3.check_number, 3);
        assert!(r3.is_final_check);
        assert_eq!(r3.new_status, Some(RegressionWatchStatus::Resolved));
    }

    #[tokio::test]
    async fn test_scenario_regression_detected_early() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10,
            monitoring_duration_secs: 50, // 5 checks max
            ..Default::default()
        };

        let (watch_id, _) = setup_watch(&tracker, "sentry", "early-reg", "ER-1", 1);

        // Check 1 - no regression
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config.clone());
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let r1 = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert!(!r1.regression_detected);
        assert!(r1.new_status.is_none());

        // Check 2 - regression detected!
        let checker2 = MockChecker::new(true);
        let scheduler2 = RegressionScheduler::new(checker2, tracker.clone(), config);
        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let r2 = scheduler2.check_watch(&stored).await.unwrap().unwrap();
        assert!(r2.regression_detected);
        assert_eq!(r2.new_status, Some(RegressionWatchStatus::Regressed));

        // Status is now Regressed, so further check_monitoring_watches should not pick it up
        let checker3 = MockChecker::new(false);
        let scheduler3 = RegressionScheduler::new(
            checker3,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );
        let results = scheduler3.check_monitoring_watches().await.unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_scenario_single_check_window() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 100,
            monitoring_duration_secs: 100, // Only 1 check
            ..Default::default()
        };

        let (_watch_id, stored) = setup_watch(&tracker, "sentry", "single-window", "SW-1", 1);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.check_number, 1);
        assert!(result.is_final_check);
        assert_eq!(result.new_status, Some(RegressionWatchStatus::Resolved));
    }

    #[tokio::test]
    async fn test_check_watch_with_zero_interval_config() {
        // Zero interval means max(1) is used, so monitoring_duration/1 = many checks
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 0,
            monitoring_duration_secs: 5,
            ..Default::default()
        };

        let (_, stored) = setup_watch(&tracker, "sentry", "zero-int", "ZI-1", 1);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // With interval 0 -> max(1) = 1, required_secs = 1*1 = 1 second
        // Started 1 hour ago, so definitely past 1 second
        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.check_number, 1);
        // max_checks = 5/1 = 5
        assert!(!r.is_final_check);
    }

    #[tokio::test]
    async fn test_check_watch_large_number_of_prior_checks() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 1,
            monitoring_duration_secs: 1000, // 1000 checks
            ..Default::default()
        };

        let (watch_id, _) = setup_watch(&tracker, "sentry", "large-checks", "LCH-1", 2);

        // Record 999 prior checks
        for _ in 0..999 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // Check 1000 is the final check (1000/1 = 1000 max)
        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.check_number, 1000);
        assert!(result.is_final_check);
        assert_eq!(result.new_status, Some(RegressionWatchStatus::Resolved));
    }

    #[tokio::test]
    async fn test_check_watch_timing_boundary_exact_seconds() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 3600,
            monitoring_duration_secs: 7200, // 2 checks
            ..Default::default()
        };

        // Started exactly 3600 seconds ago
        {
            tracker
                .record_attempt("sentry", "exact-3600", "E36-1")
                .unwrap();
            tracker
                .mark_success(
                    "sentry",
                    "exact-3600",
                    "https://github.com/org/repo/pull/500",
                )
                .unwrap();
            let attempt = tracker
                .get_attempt("sentry", "exact-3600")
                .unwrap()
                .unwrap();

            let mut watch = RegressionWatch::new(IssueType::SentryIssue, "exact-3600", attempt.id);
            watch.status = RegressionWatchStatus::Monitoring;
            watch.monitoring_started_at = Some(Utc::now() - Duration::seconds(3600));
            tracker.create_regression_watch(&watch).unwrap();
        }

        let watches = tracker
            .get_regression_watches_by_status(RegressionWatchStatus::Monitoring)
            .unwrap();
        assert!(!watches.is_empty());

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        let results = scheduler.check_monitoring_watches().await.unwrap();
        // 3600 seconds elapsed >= 3600 required for check 1
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].check_number, 1);
    }

    #[tokio::test]
    async fn test_check_monitoring_watches_with_no_monitoring_started() {
        let tracker = create_test_tracker();

        // Create a watch with monitoring status but no monitoring_started_at
        tracker.record_attempt("sentry", "no-ms", "NMS-1").unwrap();
        tracker
            .mark_success("sentry", "no-ms", "https://github.com/org/repo/pull/600")
            .unwrap();
        let attempt = tracker.get_attempt("sentry", "no-ms").unwrap().unwrap();

        let mut watch = RegressionWatch::new(IssueType::SentryIssue, "no-ms", attempt.id);
        watch.status = RegressionWatchStatus::Monitoring;
        // Leave monitoring_started_at as None
        tracker.create_regression_watch(&watch).unwrap();

        let checker = MockChecker::new(false);
        let scheduler =
            RegressionScheduler::new(checker, tracker, RegressionSchedulerConfig::default());

        let results = scheduler.check_monitoring_watches().await.unwrap();
        // check_watch returns None for None monitoring_started_at => empty results
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_check_watch_second_check_timing() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 3600,
            monitoring_duration_secs: 86400,
            ..Default::default()
        };

        // Started 5 hours ago
        let (watch_id, _) = setup_watch(&tracker, "sentry", "second-timing", "ST-1", 5);

        // Add 1 prior check
        let check = RegressionCheck::new(watch_id, false);
        tracker.record_regression_check(&check).unwrap();

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // check_number = 2, required_secs = 2 * 3600 = 7200s = 2 hours
        // 5 hours have passed > 2 hours, so check should proceed
        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().check_number, 2);
    }

    #[tokio::test]
    async fn test_check_watch_second_check_not_due_timing() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 3600,
            monitoring_duration_secs: 86400,
            ..Default::default()
        };

        // Started 1.5 hours ago (5400 seconds)
        {
            tracker
                .record_attempt("sentry", "notdue-2nd", "ND2-1")
                .unwrap();
            tracker
                .mark_success(
                    "sentry",
                    "notdue-2nd",
                    "https://github.com/org/repo/pull/700",
                )
                .unwrap();
            let attempt = tracker
                .get_attempt("sentry", "notdue-2nd")
                .unwrap()
                .unwrap();

            let mut watch = RegressionWatch::new(IssueType::SentryIssue, "notdue-2nd", attempt.id);
            watch.status = RegressionWatchStatus::Monitoring;
            watch.monitoring_started_at = Some(Utc::now() - Duration::seconds(5400));
            let watch_id = tracker.create_regression_watch(&watch).unwrap();

            // Add 1 prior check
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();

            let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
            let checker = MockChecker::new(false);
            let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

            // check_number = 2, required_secs = 2 * 3600 = 7200
            // 5400 seconds have passed < 7200 required
            let result = scheduler.check_watch(&stored).await.unwrap();
            assert!(result.is_none());
        }
    }

    #[tokio::test]
    async fn test_check_watch_watch_id_in_result() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "id-check", "IC-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.watch_id, watch_id);
    }

    #[tokio::test]
    async fn test_check_watch_records_check_in_db() {
        let tracker = create_test_tracker();
        let (watch_id, stored) = setup_watch(&tracker, "sentry", "db-check", "DC-1", 2);

        // Confirm no checks exist initially
        let initial_checks = tracker.get_regression_checks(watch_id).unwrap();
        assert!(initial_checks.is_empty());

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        scheduler.check_watch(&stored).await.unwrap();

        let checks_after = tracker.get_regression_checks(watch_id).unwrap();
        assert_eq!(checks_after.len(), 1);
        assert_eq!(checks_after[0].regression_watch_id, watch_id);
    }

    // ----- Edge case: config with very large values -----

    #[test]
    fn test_config_large_values() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: u64::MAX / 2,
            monitoring_duration_secs: u64::MAX,
            sentry_event_threshold: u32::MAX,
            similarity_threshold: f64::MAX,
        };
        // Just ensure it doesn't panic
        let _ = format!("{:?}", config);
        assert_eq!(config.sentry_event_threshold, u32::MAX);
    }

    #[test]
    fn test_config_zero_duration() {
        let config = RegressionSchedulerConfig {
            check_interval_secs: 100,
            monitoring_duration_secs: 0,
            ..Default::default()
        };
        let max_checks = config.monitoring_duration_secs / config.check_interval_secs.max(1);
        assert_eq!(max_checks, 0);
    }

    #[tokio::test]
    async fn test_check_watch_zero_duration_means_no_checks() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 100,
            monitoring_duration_secs: 0, // max_checks = 0
            ..Default::default()
        };

        let (_, stored) = setup_watch(&tracker, "sentry", "zero-dur", "ZD-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // max_checks = 0, check_number = 1, 1 > 0 so it returns None
        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_watch_interval_larger_than_duration_no_checks() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 100000,
            monitoring_duration_secs: 1000, // max_checks = 0
            ..Default::default()
        };

        let (_, stored) = setup_watch(&tracker, "sentry", "big-int", "BI-1", 100);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        // max_checks = 1000/100000 = 0, check_number(1) > 0 => None
        let result = scheduler.check_watch(&stored).await.unwrap();
        assert!(result.is_none());
    }

    // ----- Test with different issue types -----

    #[tokio::test]
    async fn test_sentry_issue_type_preserved_in_result() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "sentry", "s-preserve", "SP-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.issue_type, IssueType::SentryIssue);
    }

    #[tokio::test]
    async fn test_linear_issue_type_preserved_in_result() {
        let tracker = create_test_tracker();
        let (_, stored) = setup_watch(&tracker, "linear", "l-preserve", "LP-1", 2);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(
            checker,
            tracker.clone(),
            RegressionSchedulerConfig::default(),
        );

        let result = scheduler.check_watch(&stored).await.unwrap().unwrap();
        assert_eq!(result.issue_type, IssueType::LinearBug);
    }

    // ----- Test check details format across custom configs -----

    #[tokio::test]
    async fn test_check_details_format_custom_max_checks() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10,
            monitoring_duration_secs: 50, // 5 checks
            ..Default::default()
        };

        let (watch_id, stored) = setup_watch(&tracker, "sentry", "details-custom", "DCU-1", 1);

        let checker = MockChecker::new(false);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        scheduler.check_watch(&stored).await.unwrap();

        let checks = tracker.get_regression_checks(watch_id).unwrap();
        let details = checks[0].check_details.as_ref().unwrap();
        assert!(details.contains("Check 1/5"));
    }

    #[tokio::test]
    async fn test_check_details_includes_check_number_and_max() {
        let tracker = create_test_tracker();
        let config = RegressionSchedulerConfig {
            check_interval_secs: 10,
            monitoring_duration_secs: 100, // 10 checks
            ..Default::default()
        };

        let (watch_id, _) = setup_watch(&tracker, "sentry", "details-num", "DNM-1", 1);

        // Add 6 prior checks
        for _ in 0..6 {
            let check = RegressionCheck::new(watch_id, false);
            tracker.record_regression_check(&check).unwrap();
        }

        let stored = tracker.get_regression_watch(watch_id).unwrap().unwrap();
        let checker = MockChecker::new(true);
        let scheduler = RegressionScheduler::new(checker, tracker.clone(), config);

        scheduler.check_watch(&stored).await.unwrap();

        let checks = tracker.get_regression_checks(watch_id).unwrap();
        // Total checks should now be 7
        assert_eq!(checks.len(), 7);
        // Find the check with details (the one created by check_watch, not the bare prior checks)
        let detailed_check = checks
            .iter()
            .find(|c| c.check_details.is_some())
            .expect("Expected a check with details");
        let details = detailed_check.check_details.as_ref().unwrap();
        assert!(details.contains("Check 7/10"));
        assert!(details.contains("Regression detected"));
    }
}

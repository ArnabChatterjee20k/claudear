//! Report generation logic.

use crate::error::Result;
use crate::storage::FixAttemptTracker;
use crate::types::FixAttemptStatus;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;

/// A recurring issue pattern.
#[derive(Debug, Clone, Serialize)]
pub struct RecurringIssue {
    /// Pattern or category of the issue
    pub pattern: String,
    /// Number of occurrences
    pub count: usize,
    /// Sources where this pattern appears
    pub sources: Vec<String>,
}

/// A generated report.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Human-readable period description
    pub period: String,
    /// Start of the report period
    pub from: DateTime<Utc>,
    /// End of the report period
    pub to: DateTime<Utc>,
    /// Total issues attempted
    pub issues_attempted: usize,
    /// Issues that succeeded (PR created or merged)
    pub issues_succeeded: usize,
    /// Issues that failed
    pub issues_failed: usize,
    /// Issues marked as cannot fix
    pub issues_cannot_fix: usize,
    /// Success rate as percentage
    pub success_rate: f64,
    /// Failure rate as percentage
    pub failure_rate: f64,
    /// PRs created
    pub prs_created: usize,
    /// PRs merged
    pub prs_merged: usize,
    /// PRs closed without merge
    pub prs_closed: usize,
    /// Breakdown by source
    pub by_source: HashMap<String, SourceReport>,
    /// Issues currently pending
    pub pending_count: usize,
    /// Issues ready for retry
    pub retryable_count: usize,
}

/// Report statistics for a single source.
#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub name: String,
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub merged: usize,
}

/// Generates reports from tracking data.
pub struct ReportGenerator {
    tracker: Arc<dyn FixAttemptTracker>,
}

impl ReportGenerator {
    /// Create a new report generator.
    pub fn new(tracker: Arc<dyn FixAttemptTracker>) -> Self {
        Self { tracker }
    }

    /// Generate a report for the specified time period.
    pub fn generate(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Report> {
        let _stats = self.tracker.get_stats()?;

        // Get attempts by status
        let pending = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::Pending)?;
        let success = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::Success)?;
        let failed = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::Failed)?;
        let merged = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::Merged)?;
        let closed = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::Closed)?;
        let cannot_fix = self
            .tracker
            .get_attempts_by_status(FixAttemptStatus::CannotFix)?;

        // Filter to time period
        let in_period = |attempt: &crate::types::FixAttempt| {
            attempt.attempted_at >= from && attempt.attempted_at <= to
        };

        let period_success: Vec<_> = success.into_iter().filter(|a| in_period(a)).collect();
        let period_failed: Vec<_> = failed.into_iter().filter(|a| in_period(a)).collect();
        let period_merged: Vec<_> = merged.into_iter().filter(|a| in_period(a)).collect();
        let period_closed: Vec<_> = closed.into_iter().filter(|a| in_period(a)).collect();
        let period_cannot_fix: Vec<_> = cannot_fix.into_iter().filter(|a| in_period(a)).collect();

        // Count by source
        let mut by_source: HashMap<String, SourceReport> = HashMap::new();

        for attempt in &period_success {
            let entry = by_source
                .entry(attempt.source.clone())
                .or_insert_with(|| SourceReport {
                    name: attempt.source.clone(),
                    attempted: 0,
                    succeeded: 0,
                    failed: 0,
                    merged: 0,
                });
            entry.attempted += 1;
            entry.succeeded += 1;
        }

        for attempt in &period_merged {
            let entry = by_source
                .entry(attempt.source.clone())
                .or_insert_with(|| SourceReport {
                    name: attempt.source.clone(),
                    attempted: 0,
                    succeeded: 0,
                    failed: 0,
                    merged: 0,
                });
            entry.attempted += 1;
            entry.succeeded += 1;
            entry.merged += 1;
        }

        for attempt in &period_failed {
            let entry = by_source
                .entry(attempt.source.clone())
                .or_insert_with(|| SourceReport {
                    name: attempt.source.clone(),
                    attempted: 0,
                    succeeded: 0,
                    failed: 0,
                    merged: 0,
                });
            entry.attempted += 1;
            entry.failed += 1;
        }

        for attempt in &period_cannot_fix {
            let entry = by_source
                .entry(attempt.source.clone())
                .or_insert_with(|| SourceReport {
                    name: attempt.source.clone(),
                    attempted: 0,
                    succeeded: 0,
                    failed: 0,
                    merged: 0,
                });
            entry.attempted += 1;
            entry.failed += 1;
        }

        for attempt in &period_closed {
            let entry = by_source
                .entry(attempt.source.clone())
                .or_insert_with(|| SourceReport {
                    name: attempt.source.clone(),
                    attempted: 0,
                    succeeded: 0,
                    failed: 0,
                    merged: 0,
                });
            entry.attempted += 1;
        }

        // Calculate totals
        let issues_succeeded = period_success.len() + period_merged.len();
        let issues_failed = period_failed.len() + period_cannot_fix.len();
        let issues_attempted = issues_succeeded + issues_failed + period_closed.len();

        let success_rate = if issues_attempted > 0 {
            issues_succeeded as f64 / issues_attempted as f64 * 100.0
        } else {
            0.0
        };

        let failure_rate = if issues_attempted > 0 {
            issues_failed as f64 / issues_attempted as f64 * 100.0
        } else {
            0.0
        };

        // Get retryable issues
        let retryable = self.tracker.get_retryable_issues(2).unwrap_or_default();

        // Generate period description
        let period = Self::describe_period(from, to);

        Ok(Report {
            period,
            from,
            to,
            issues_attempted,
            issues_succeeded,
            issues_failed,
            issues_cannot_fix: period_cannot_fix.len(),
            success_rate,
            failure_rate,
            prs_created: period_success.len() + period_merged.len() + period_closed.len(),
            prs_merged: period_merged.len(),
            prs_closed: period_closed.len(),
            by_source,
            pending_count: pending.len(),
            retryable_count: retryable.len(),
        })
    }

    /// Generate a daily report (last 24 hours).
    pub fn generate_daily(&self) -> Result<Report> {
        let to = Utc::now();
        let from = to - Duration::days(1);
        self.generate(from, to)
    }

    /// Generate a weekly report (last 7 days).
    pub fn generate_weekly(&self) -> Result<Report> {
        let to = Utc::now();
        let from = to - Duration::days(7);
        self.generate(from, to)
    }

    /// Generate a monthly report (last 30 days).
    pub fn generate_monthly(&self) -> Result<Report> {
        let to = Utc::now();
        let from = to - Duration::days(30);
        self.generate(from, to)
    }

    /// Generate an all-time report.
    pub fn generate_all_time(&self) -> Result<Report> {
        let to = Utc::now();
        let from = DateTime::from_timestamp(0, 0).unwrap_or(to);
        self.generate(from, to)
    }

    /// Describe the time period in human-readable form.
    fn describe_period(from: DateTime<Utc>, to: DateTime<Utc>) -> String {
        let duration = to - from;
        let days = duration.num_days();

        if days <= 1 {
            "Last 24 Hours".to_string()
        } else if days <= 7 {
            "Last 7 Days".to_string()
        } else if days <= 30 {
            "Last 30 Days".to_string()
        } else {
            format!("{} to {}", from.format("%Y-%m-%d"), to.format("%Y-%m-%d"))
        }
    }
}

impl Report {
    /// Format the report as a plain text summary.
    pub fn format_text(&self) -> String {
        let mut text = String::new();

        text.push_str(&format!("# Report: {}\n", self.period));
        text.push_str(&format!(
            "Period: {} to {}\n\n",
            self.from.format("%Y-%m-%d %H:%M UTC"),
            self.to.format("%Y-%m-%d %H:%M UTC")
        ));

        text.push_str("## Summary\n");
        text.push_str(&format!("- Issues Attempted: {}\n", self.issues_attempted));
        text.push_str(&format!(
            "- Succeeded: {} ({:.1}%)\n",
            self.issues_succeeded, self.success_rate
        ));
        text.push_str(&format!(
            "- Failed: {} ({:.1}%)\n",
            self.issues_failed, self.failure_rate
        ));
        text.push_str(&format!("- Cannot Fix: {}\n", self.issues_cannot_fix));
        text.push('\n');

        text.push_str("## Pull Requests\n");
        text.push_str(&format!("- Created: {}\n", self.prs_created));
        text.push_str(&format!("- Merged: {}\n", self.prs_merged));
        text.push_str(&format!("- Closed: {}\n", self.prs_closed));
        text.push('\n');

        text.push_str("## Current Status\n");
        text.push_str(&format!("- Pending: {}\n", self.pending_count));
        text.push_str(&format!("- Retryable: {}\n", self.retryable_count));
        text.push('\n');

        if !self.by_source.is_empty() {
            text.push_str("## By Source\n");
            for (name, source) in &self.by_source {
                text.push_str(&format!(
                    "- {}: {} attempted, {} succeeded, {} failed, {} merged\n",
                    name, source.attempted, source.succeeded, source.failed, source.merged
                ));
            }
        }

        text
    }

    /// Format the report as a brief summary for SMS/Push.
    pub fn format_brief(&self) -> String {
        format!(
            "Report: {} | {} attempted, {:.0}% success, {} merged, {} pending",
            self.period,
            self.issues_attempted,
            self.success_rate,
            self.prs_merged,
            self.pending_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::SqliteTracker;
    use tempfile::TempDir;

    fn create_test_tracker() -> (TempDir, Arc<dyn FixAttemptTracker>) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let tracker = SqliteTracker::new(db_path.to_str().unwrap()).unwrap();
        (temp_dir, Arc::new(tracker))
    }

    #[test]
    fn test_generate_empty_report() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let to = Utc::now();
        let from = to - Duration::days(1);
        let report = generator.generate(from, to).unwrap();

        assert_eq!(report.issues_attempted, 0);
        assert_eq!(report.issues_succeeded, 0);
        assert_eq!(report.issues_failed, 0);
        assert_eq!(report.success_rate, 0.0);
    }

    #[test]
    fn test_generate_daily_report() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        assert!(report.period.contains("24 Hours"));
    }

    #[test]
    fn test_generate_weekly_report() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_weekly().unwrap();
        assert!(report.period.contains("7 Days"));
    }

    #[test]
    fn test_format_text() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        let text = report.format_text();

        assert!(text.contains("Report:"));
        assert!(text.contains("Summary"));
        assert!(text.contains("Pull Requests"));
    }

    #[test]
    fn test_format_brief() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        let brief = report.format_brief();

        assert!(brief.contains("attempted"));
        assert!(brief.contains("success"));
        assert!(brief.contains("merged"));
    }

    #[test]
    fn test_generate_monthly_report() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_monthly().unwrap();
        assert!(report.period.contains("30 Days"));
    }

    #[test]
    fn test_generate_all_time_report() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_all_time().unwrap();
        // All-time should have a date range format
        assert!(report.period.contains("to") || report.period.contains("Days"));
    }

    #[test]
    fn test_describe_period_24_hours() {
        let now = Utc::now();
        let from = now - Duration::hours(12);
        let period = ReportGenerator::describe_period(from, now);
        assert!(period.contains("24 Hours"));
    }

    #[test]
    fn test_describe_period_7_days() {
        let now = Utc::now();
        let from = now - Duration::days(5);
        let period = ReportGenerator::describe_period(from, now);
        assert!(period.contains("7 Days"));
    }

    #[test]
    fn test_describe_period_30_days() {
        let now = Utc::now();
        let from = now - Duration::days(15);
        let period = ReportGenerator::describe_period(from, now);
        assert!(period.contains("30 Days"));
    }

    #[test]
    fn test_describe_period_custom_range() {
        let now = Utc::now();
        let from = now - Duration::days(60);
        let period = ReportGenerator::describe_period(from, now);
        assert!(period.contains("to"));
    }

    #[test]
    fn test_report_fields() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let to = Utc::now();
        let from = to - Duration::days(1);
        let report = generator.generate(from, to).unwrap();

        assert_eq!(report.issues_attempted, 0);
        assert_eq!(report.issues_succeeded, 0);
        assert_eq!(report.issues_failed, 0);
        assert_eq!(report.issues_cannot_fix, 0);
        assert_eq!(report.success_rate, 0.0);
        assert_eq!(report.failure_rate, 0.0);
        assert_eq!(report.prs_created, 0);
        assert_eq!(report.prs_merged, 0);
        assert_eq!(report.prs_closed, 0);
    }

    #[test]
    fn test_format_text_sections() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        let text = report.format_text();

        assert!(text.contains("# Report:"));
        assert!(text.contains("## Summary"));
        assert!(text.contains("Issues Attempted:"));
        assert!(text.contains("Succeeded:"));
        assert!(text.contains("Failed:"));
        assert!(text.contains("Cannot Fix:"));
        assert!(text.contains("## Pull Requests"));
        assert!(text.contains("Created:"));
        assert!(text.contains("Merged:"));
        assert!(text.contains("Closed:"));
        assert!(text.contains("## Current Status"));
        assert!(text.contains("Pending:"));
        assert!(text.contains("Retryable:"));
    }

    #[test]
    fn test_source_report_fields() {
        let source_report = SourceReport {
            name: "linear".to_string(),
            attempted: 10,
            succeeded: 7,
            failed: 3,
            merged: 5,
        };

        assert_eq!(source_report.name, "linear");
        assert_eq!(source_report.attempted, 10);
        assert_eq!(source_report.succeeded, 7);
        assert_eq!(source_report.failed, 3);
        assert_eq!(source_report.merged, 5);
    }

    #[test]
    fn test_recurring_issue_fields() {
        let issue = RecurringIssue {
            pattern: "NullPointerException".to_string(),
            count: 5,
            sources: vec!["sentry".to_string(), "linear".to_string()],
        };

        assert_eq!(issue.pattern, "NullPointerException");
        assert_eq!(issue.count, 5);
        assert_eq!(issue.sources.len(), 2);
    }

    #[test]
    fn test_report_serialization() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        let json = serde_json::to_string(&report).unwrap();

        assert!(json.contains("period"));
        assert!(json.contains("issues_attempted"));
        assert!(json.contains("success_rate"));
    }

    #[test]
    fn test_source_report_serialization() {
        let source_report = SourceReport {
            name: "linear".to_string(),
            attempted: 10,
            succeeded: 7,
            failed: 3,
            merged: 5,
        };

        let json = serde_json::to_string(&source_report).unwrap();
        assert!(json.contains("linear"));
        assert!(json.contains("10"));
    }

    #[test]
    fn test_report_date_format() {
        let (_temp, tracker) = create_test_tracker();
        let generator = ReportGenerator::new(tracker);

        let report = generator.generate_daily().unwrap();
        let text = report.format_text();

        // Should contain UTC date format
        assert!(text.contains("UTC"));
    }
}

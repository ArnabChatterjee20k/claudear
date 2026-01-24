//! Analytics-specific queries and aggregation functions.
//!
//! This module provides higher-level analytics functionality built on top of
//! the core storage layer, including trend analysis, success rate calculations,
//! and time-series data retrieval.

use crate::error::Result;
use crate::types::{AnalyticsSummary, ErrorPattern, ProcessingMetric};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use super::SqliteTracker;

/// Time period for trend analysis.
#[derive(Debug, Clone, Copy)]
pub enum TimePeriod {
    /// Last hour
    Hour,
    /// Last 24 hours
    Day,
    /// Last 7 days
    Week,
    /// Last 30 days
    Month,
}

impl TimePeriod {
    /// Get the duration for this time period.
    pub fn duration(&self) -> Duration {
        match self {
            TimePeriod::Hour => Duration::hours(1),
            TimePeriod::Day => Duration::days(1),
            TimePeriod::Week => Duration::days(7),
            TimePeriod::Month => Duration::days(30),
        }
    }

    /// Get the start time for this period from now.
    pub fn start_time(&self) -> DateTime<Utc> {
        Utc::now() - self.duration()
    }
}

/// Trend direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrendDirection {
    Up,
    Down,
    Stable,
}

/// Trend analysis result.
#[derive(Debug, Clone)]
pub struct TrendAnalysis {
    /// Current value
    pub current: f64,
    /// Previous value (for comparison period)
    pub previous: f64,
    /// Percentage change
    pub change_percent: f64,
    /// Trend direction
    pub direction: TrendDirection,
}

impl TrendAnalysis {
    /// Create a new trend analysis.
    pub fn new(current: f64, previous: f64) -> Self {
        let change_percent = if previous > 0.0 {
            ((current - previous) / previous) * 100.0
        } else if current > 0.0 {
            100.0
        } else {
            0.0
        };

        let direction = if change_percent > 5.0 {
            TrendDirection::Up
        } else if change_percent < -5.0 {
            TrendDirection::Down
        } else {
            TrendDirection::Stable
        };

        Self {
            current,
            previous,
            change_percent,
            direction,
        }
    }
}

/// Analytics service for querying and analyzing operational data.
pub struct AnalyticsService<'a> {
    tracker: &'a SqliteTracker,
}

impl<'a> AnalyticsService<'a> {
    /// Create a new analytics service.
    pub fn new(tracker: &'a SqliteTracker) -> Self {
        Self { tracker }
    }

    /// Get the overall analytics summary.
    pub fn get_summary(&self) -> Result<AnalyticsSummary> {
        self.tracker.get_analytics_summary()
    }

    /// Get the success rate over a time period.
    pub fn get_success_rate(&self) -> Result<f64> {
        self.tracker.get_success_rate()
    }

    /// Get recent metrics for a given metric name.
    pub fn get_recent_metrics(
        &self,
        metric_name: &str,
        period: TimePeriod,
    ) -> Result<Vec<ProcessingMetric>> {
        let since = Some(period.start_time());
        self.tracker.get_metrics(metric_name, since, 1000)
    }

    /// Get the most common error patterns.
    pub fn get_top_errors(&self, limit: usize) -> Result<Vec<ErrorPattern>> {
        self.tracker.get_error_patterns(limit)
    }

    /// Calculate the average metric value over a time period.
    pub fn average_metric(&self, metric_name: &str, period: TimePeriod) -> Result<Option<f64>> {
        let metrics = self.get_recent_metrics(metric_name, period)?;
        if metrics.is_empty() {
            return Ok(None);
        }

        let sum: f64 = metrics.iter().map(|m| m.metric_value).sum();
        Ok(Some(sum / metrics.len() as f64))
    }

    /// Analyze the trend for a metric by comparing two time periods.
    pub fn analyze_metric_trend(
        &self,
        metric_name: &str,
        period: TimePeriod,
    ) -> Result<Option<TrendAnalysis>> {
        let now = Utc::now();
        let period_duration = period.duration();

        // Current period
        let current_start = now - period_duration;
        let current_metrics = self
            .tracker
            .get_metrics(metric_name, Some(current_start), 1000)?;

        // Previous period
        let previous_start = current_start - period_duration;
        let previous_metrics = self
            .tracker
            .get_metrics(metric_name, Some(previous_start), 1000)?;

        // Filter previous metrics to only include those before current period
        let previous_metrics: Vec<_> = previous_metrics
            .into_iter()
            .filter(|m| m.timestamp < current_start)
            .collect();

        if current_metrics.is_empty() && previous_metrics.is_empty() {
            return Ok(None);
        }

        let current_avg = if current_metrics.is_empty() {
            0.0
        } else {
            current_metrics.iter().map(|m| m.metric_value).sum::<f64>()
                / current_metrics.len() as f64
        };

        let previous_avg = if previous_metrics.is_empty() {
            0.0
        } else {
            previous_metrics.iter().map(|m| m.metric_value).sum::<f64>()
                / previous_metrics.len() as f64
        };

        Ok(Some(TrendAnalysis::new(current_avg, previous_avg)))
    }

    /// Get metrics aggregated by source.
    pub fn metrics_by_source(
        &self,
        metric_name: &str,
        period: TimePeriod,
    ) -> Result<HashMap<String, f64>> {
        let metrics = self.get_recent_metrics(metric_name, period)?;

        let mut by_source: HashMap<String, Vec<f64>> = HashMap::new();
        for metric in metrics {
            if let Some(source) = metric.source {
                by_source
                    .entry(source)
                    .or_default()
                    .push(metric.metric_value);
            }
        }

        let mut result = HashMap::new();
        for (source, values) in by_source {
            if !values.is_empty() {
                let avg = values.iter().sum::<f64>() / values.len() as f64;
                result.insert(source, avg);
            }
        }

        Ok(result)
    }

    /// Calculate throughput (issues processed per hour) over a time period.
    pub fn calculate_throughput(&self, period: TimePeriod) -> Result<f64> {
        let activities = self.tracker.get_recent_activities(10000, None)?;

        let start_time = period.start_time();
        let processing_count = activities
            .iter()
            .filter(|a| {
                a.timestamp >= start_time
                    && (a.activity_type == "processing_completed"
                        || a.activity_type == "pr_created")
            })
            .count();

        let hours = period.duration().num_hours() as f64;
        if hours > 0.0 {
            Ok(processing_count as f64 / hours)
        } else {
            Ok(0.0)
        }
    }

    /// Get error rate (errors per total processing) over a time period.
    pub fn calculate_error_rate(&self, period: TimePeriod) -> Result<f64> {
        let activities = self.tracker.get_recent_activities(10000, None)?;
        let start_time = period.start_time();

        let total = activities
            .iter()
            .filter(|a| {
                a.timestamp >= start_time
                    && (a.activity_type == "processing_completed"
                        || a.activity_type == "error"
                        || a.activity_type == "pr_created")
            })
            .count();

        let errors = activities
            .iter()
            .filter(|a| a.timestamp >= start_time && a.activity_type == "error")
            .count();

        if total > 0 {
            Ok(errors as f64 / total as f64)
        } else {
            Ok(0.0)
        }
    }
}

/// Helper function to compute a hash for error message normalization.
pub fn compute_error_hash(error_message: &str) -> String {
    use sha2::{Digest, Sha256};

    // Normalize the error message by:
    // 1. Converting to lowercase
    // 2. Removing numbers (often variable)
    // 3. Removing extra whitespace
    let normalized: String = error_message
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_numeric())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    hex::encode(&hasher.finalize()[..16]) // Use first 16 bytes for shorter hash
}

/// Classify an error message into an error type.
pub fn classify_error(error_message: &str) -> &'static str {
    let lower = error_message.to_lowercase();

    if lower.contains("timeout") || lower.contains("timed out") {
        "timeout"
    } else if lower.contains("build") || lower.contains("compile") || lower.contains("cargo") {
        "build_failure"
    } else if lower.contains("test") || lower.contains("assertion") {
        "test_failure"
    } else if lower.contains("claude") || lower.contains("api") || lower.contains("rate limit") {
        "claude_error"
    } else if lower.contains("git") || lower.contains("merge") || lower.contains("conflict") {
        "git_error"
    } else if lower.contains("permission") || lower.contains("access denied") {
        "permission_error"
    } else if lower.contains("network") || lower.contains("connection") {
        "network_error"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_time_period_duration() {
        assert_eq!(TimePeriod::Hour.duration().num_hours(), 1);
        assert_eq!(TimePeriod::Day.duration().num_days(), 1);
        assert_eq!(TimePeriod::Week.duration().num_days(), 7);
        assert_eq!(TimePeriod::Month.duration().num_days(), 30);
    }

    #[test]
    fn test_trend_analysis_up() {
        let trend = TrendAnalysis::new(120.0, 100.0);
        assert_eq!(trend.direction, TrendDirection::Up);
        assert!((trend.change_percent - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_trend_analysis_down() {
        let trend = TrendAnalysis::new(80.0, 100.0);
        assert_eq!(trend.direction, TrendDirection::Down);
        assert!((trend.change_percent - (-20.0)).abs() < 0.01);
    }

    #[test]
    fn test_trend_analysis_stable() {
        let trend = TrendAnalysis::new(101.0, 100.0);
        assert_eq!(trend.direction, TrendDirection::Stable);
    }

    #[test]
    fn test_trend_analysis_from_zero() {
        let trend = TrendAnalysis::new(100.0, 0.0);
        assert_eq!(trend.direction, TrendDirection::Up);
        assert_eq!(trend.change_percent, 100.0);
    }

    #[test]
    fn test_compute_error_hash() {
        let hash1 = compute_error_hash("Error at line 42: undefined variable");
        let hash2 = compute_error_hash("Error at line 100: undefined variable");
        // Should produce the same hash (numbers removed)
        assert_eq!(hash1, hash2);

        let hash3 = compute_error_hash("Different error message");
        assert_ne!(hash1, hash3);
    }

    #[test]
    fn test_classify_error() {
        assert_eq!(
            classify_error("Process timed out after 60 seconds"),
            "timeout"
        );
        assert_eq!(
            classify_error("Build failed: cargo build error"),
            "build_failure"
        );
        assert_eq!(classify_error("Test assertion failed"), "test_failure");
        assert_eq!(
            classify_error("Claude API rate limit exceeded"),
            "claude_error"
        );
        assert_eq!(classify_error("Git merge conflict"), "git_error");
        assert_eq!(classify_error("Permission denied"), "permission_error");
        assert_eq!(
            classify_error("Network connection refused"),
            "network_error"
        );
        assert_eq!(classify_error("Some random error"), "unknown");
    }

    #[test]
    fn test_analytics_service_creation() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let _service = AnalyticsService::new(&tracker);
    }

    #[test]
    fn test_analytics_summary() {
        let tracker = SqliteTracker::in_memory().unwrap();
        let service = AnalyticsService::new(&tracker);

        let summary = service.get_summary().unwrap();
        assert_eq!(summary.total_processed, 0);
        assert_eq!(summary.success_rate, 0.0);
    }
}

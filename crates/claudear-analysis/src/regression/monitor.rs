//! Regression checker trait and implementations.
//!
//! Defines how to check for regressions based on issue type.

use async_trait::async_trait;
use claudear_core::error::Result;
use claudear_core::types::RegressionWatch;

/// Result of a regression check.
#[derive(Debug, Clone)]
pub struct RegressionResult {
    /// Whether a regression was detected.
    pub regression_detected: bool,
    /// Optional details about the check.
    pub details: Option<String>,
}

impl RegressionResult {
    /// Create a result indicating no regression.
    pub fn no_regression() -> Self {
        Self {
            regression_detected: false,
            details: None,
        }
    }

    /// Create a result indicating regression detected.
    pub fn regression(details: impl Into<String>) -> Self {
        Self {
            regression_detected: true,
            details: Some(details.into()),
        }
    }
}

/// Trait for checking regressions.
#[async_trait]
pub trait RegressionChecker: Send + Sync {
    /// Check if a regression has occurred for the given watch.
    async fn check_regression(&self, watch: &RegressionWatch) -> Result<RegressionResult>;
}

/// A composite checker that uses different strategies based on issue type.
pub struct CompositeChecker {
    sentry_checker: Box<dyn RegressionChecker>,
    linear_checker: Box<dyn RegressionChecker>,
}

impl CompositeChecker {
    /// Create a new composite checker.
    pub fn new(
        sentry_checker: Box<dyn RegressionChecker>,
        linear_checker: Box<dyn RegressionChecker>,
    ) -> Self {
        Self {
            sentry_checker,
            linear_checker,
        }
    }
}

#[async_trait]
impl RegressionChecker for CompositeChecker {
    async fn check_regression(&self, watch: &RegressionWatch) -> Result<RegressionResult> {
        match watch.issue_type {
            claudear_core::types::IssueType::SentryIssue => {
                self.sentry_checker.check_regression(watch).await
            }
            claudear_core::types::IssueType::LinearBug => {
                self.linear_checker.check_regression(watch).await
            }
            claudear_core::types::IssueType::GitLabIssue
            | claudear_core::types::IssueType::JiraIssue => Ok(RegressionResult::no_regression()),
        }
    }
}

/// A no-op checker that always returns no regression.
/// Useful for testing or when a specific check type is disabled.
pub struct NoOpChecker;

#[async_trait]
impl RegressionChecker for NoOpChecker {
    async fn check_regression(&self, _watch: &RegressionWatch) -> Result<RegressionResult> {
        Ok(RegressionResult::no_regression())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claudear_core::types::IssueType;

    struct AlwaysRegressionChecker;

    #[async_trait]
    impl RegressionChecker for AlwaysRegressionChecker {
        async fn check_regression(&self, _watch: &RegressionWatch) -> Result<RegressionResult> {
            Ok(RegressionResult::regression("Always detects regression"))
        }
    }

    #[test]
    fn test_regression_result_no_regression() {
        let result = RegressionResult::no_regression();
        assert!(!result.regression_detected);
        assert!(result.details.is_none());
    }

    #[test]
    fn test_regression_result_with_regression() {
        let result = RegressionResult::regression("Issue reappeared");
        assert!(result.regression_detected);
        assert_eq!(result.details, Some("Issue reappeared".to_string()));
    }

    #[tokio::test]
    async fn test_noop_checker() {
        let checker = NoOpChecker;
        let watch = RegressionWatch::new(IssueType::SentryIssue, "test-123", 1);

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_composite_checker_sentry() {
        let sentry = Box::new(AlwaysRegressionChecker);
        let linear = Box::new(NoOpChecker);
        let composite = CompositeChecker::new(sentry, linear);

        let watch = RegressionWatch::new(IssueType::SentryIssue, "sentry-123", 1);
        let result = composite.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected); // Uses sentry checker
    }

    #[tokio::test]
    async fn test_composite_checker_linear() {
        let sentry = Box::new(AlwaysRegressionChecker);
        let linear = Box::new(NoOpChecker);
        let composite = CompositeChecker::new(sentry, linear);

        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-456", 1);
        let result = composite.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected); // Uses linear checker (no-op)
    }
}

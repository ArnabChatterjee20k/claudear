//! Regression monitoring for bug fix verification.
//!
//! This module monitors for regressions after bug fixes are deployed,
//! checking hourly for 24 hours to verify fixes are stable.

mod linear;
mod monitor;
mod scheduler;
mod sentry;

pub use linear::{LinearRegressionChecker, LinearRegressionConfig};
pub use monitor::{CompositeChecker, NoOpChecker, RegressionChecker, RegressionResult};
pub use scheduler::{CheckCycleResult, RegressionScheduler, RegressionSchedulerConfig};
pub use sentry::{SentryRegressionChecker, SentryRegressionConfig};

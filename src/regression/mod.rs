//! Regression monitoring for bug fix verification.
//!
//! This module monitors for regressions after bug fixes are deployed,
//! checking hourly for 24 hours to verify fixes are stable.

mod monitor;
mod scheduler;

pub use monitor::{RegressionChecker, RegressionResult};
pub use scheduler::{RegressionScheduler, RegressionSchedulerConfig};

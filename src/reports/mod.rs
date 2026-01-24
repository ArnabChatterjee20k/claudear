//! Scheduled reports module.
//!
//! Provides automated daily/weekly reporting via notifications.

mod generator;
mod scheduler;

pub use generator::{RecurringIssue, Report, ReportGenerator};
pub use scheduler::{ReportFrequency, ReportSchedule, ReportScheduler};

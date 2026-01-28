//! Release tracking for bug fix verification.
//!
//! This module tracks releases across the Appwrite ecosystem to detect
//! when bug fixes are included in production releases.

mod github;
mod tracker;

pub use github::ReleaseClient;
pub use tracker::{ReleaseTracker, ReleaseTrackerConfig};

//! Multi-repository support module.
//!
//! Core repo types and analysis are provided by `claudear_analysis::repo`.
//! SCM-specific index building functions are provided by `claudear_engine::repo_index`.

// Re-export everything from the analysis crate's repo module
pub use claudear_analysis::repo::*;

// Re-export SCM-specific index functions from the engine crate
pub use claudear_engine::repo_index::{
    build_repo_index_from_github, build_repo_index_from_gitlab, build_repo_index_with_fallback,
};

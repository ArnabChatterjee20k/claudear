//! Multi-repository support module.
//!
//! Provides git operations, dependency tracking, cascading changes, and repository indexing.

pub mod code_index;
mod discovery;
mod git;
pub(crate) mod index;
mod relationships;

pub use claudear_core::types::{IndexedRepo, RepoIndex};
pub use discovery::{DependencyDiscovery, DiscoveredDependency};
pub use git::{worktree_path, GitOps};
pub use index::{build_repo_index, index_repo_files};
pub use relationships::{
    CascadingChange, DependencyGraph, DependencyType, RepoRelationships, Repository,
};

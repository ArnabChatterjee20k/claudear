//! Multi-repository support module.
//!
//! Provides git operations, dependency tracking, cascading changes, and repository indexing.

pub mod code_index;
mod discovery;
mod git;
mod index;
mod relationships;

pub use discovery::{DependencyDiscovery, DiscoveredDependency};
pub use git::{worktree_path, GitOps};
pub use index::{IndexedRepo, RepoIndex};
pub use relationships::{
    CascadingChange, DependencyGraph, DependencyType, RepoRelationships, Repository,
};

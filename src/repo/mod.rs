//! Multi-repository support module.
//!
//! Provides git operations, dependency tracking, cascading changes, and repository indexing.

mod discovery;
mod git;
mod index;
mod relationships;

pub use discovery::{DependencyDiscovery, DiscoveredDependency};
pub use git::GitOps;
pub use index::{IndexedRepo, RepoIndex};
pub use relationships::{
    CascadingChange, DependencyGraph, DependencyType, RepoRelationships, Repository,
};

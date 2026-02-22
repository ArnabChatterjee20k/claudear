//! Drop-based resource cleanup for E2E tests.
//!
//! Tracks PRs, branches, and issues created during a scenario and cleans them up.

use claudear::scm::ScmProvider;
use claudear::source::IssueSource;
use std::sync::Arc;

/// Tracks resources created during a scenario for cleanup.
pub struct CleanupTracker {
    scm: Arc<dyn ScmProvider>,
    source: Arc<dyn IssueSource>,
    /// (repo, pr_number) pairs to close.
    prs: Vec<(String, i64)>,
    /// (repo, branch_name) pairs to delete.
    branches: Vec<(String, String)>,
    /// Issue IDs to resolve.
    issue_ids: Vec<String>,
}

impl CleanupTracker {
    pub fn new(scm: Arc<dyn ScmProvider>, source: Arc<dyn IssueSource>) -> Self {
        Self {
            scm,
            source,
            prs: Vec::new(),
            branches: Vec::new(),
            issue_ids: Vec::new(),
        }
    }

    pub fn track_pr(&mut self, repo: &str, number: i64) {
        self.prs.push((repo.to_string(), number));
    }

    pub fn track_branch(&mut self, repo: &str, branch: &str) {
        self.branches.push((repo.to_string(), branch.to_string()));
    }

    pub fn track_issue(&mut self, issue_id: &str) {
        self.issue_ids.push(issue_id.to_string());
    }

    /// Run cleanup synchronously in a blocking context.
    /// Call this before the scenario returns.
    pub async fn cleanup(&self) {
        tracing::info!(
            prs = self.prs.len(),
            branches = self.branches.len(),
            issues = self.issue_ids.len(),
            "Running cleanup"
        );

        for (repo, number) in &self.prs {
            match self.scm.close_pr(repo, *number).await {
                Ok(()) => tracing::info!(repo, number, "Closed PR"),
                Err(e) => tracing::warn!(repo, number, error = %e, "Failed to close PR"),
            }
        }

        for (repo, branch) in &self.branches {
            match self.scm.delete_branch(repo, branch).await {
                Ok(()) => tracing::info!(repo, branch, "Deleted branch"),
                Err(e) => tracing::warn!(repo, branch, error = %e, "Failed to delete branch"),
            }
        }

        for issue_id in &self.issue_ids {
            match self.source.resolve_issue(issue_id).await {
                Ok(()) => tracing::info!(issue_id, "Resolved issue"),
                Err(e) => tracing::warn!(issue_id, error = %e, "Failed to resolve issue"),
            }
        }
    }
}

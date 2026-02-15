//! Git operations for repository management.

use crate::error::{Error, Result};
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Git operations for managing repositories.
pub struct GitOps;

impl GitOps {
    /// Ensure a repository is available and up to date.
    ///
    /// If the repository doesn't exist locally, it will be cloned.
    /// If it exists, it will be pulled to update.
    pub async fn ensure_repo_at_path(
        repo_path: &Path,
        github_url: &str,
        default_branch: &str,
    ) -> Result<()> {
        if repo_path.exists() {
            Self::pull(repo_path, default_branch).await
        } else {
            Self::clone(github_url, repo_path).await
        }
    }

    /// Clone a repository.
    async fn clone(url: &str, target: &Path) -> Result<()> {
        tracing::info!(url = %url, target = ?target, "Cloning repository");

        // Create parent directory if it doesn't exist
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                Error::io(format!(
                    "Failed to create parent directory {:?}: {}",
                    parent, e
                ))
            })?;
        }

        // Validate the URL to prevent command injection via malicious repository URLs.
        // Reject URLs containing shell metacharacters or those starting with '-' (option injection).
        if url.starts_with('-') || url.contains(';') || url.contains('|') || url.contains('$') {
            return Err(Error::git(format!(
                "Invalid repository URL (contains disallowed characters): {}",
                url
            )));
        }

        let output = Command::new("git")
            .args(["clone", "--depth", "1", "--", url])
            .arg(target)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git clone: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git clone failed: {}", stderr)));
        }

        tracing::info!(target = ?target, "Repository cloned successfully");
        Ok(())
    }

    /// Pull latest changes on a branch.
    async fn pull(repo_path: &Path, branch: &str) -> Result<()> {
        tracing::debug!(repo = ?repo_path, branch = %branch, "Pulling latest changes");

        // Validate branch name to prevent injection via crafted branch names
        if branch.starts_with('-')
            || branch.contains(';')
            || branch.contains('|')
            || branch.contains('$')
            || branch.contains("..")
        {
            return Err(Error::git(format!(
                "Invalid branch name (contains disallowed characters): {}",
                branch
            )));
        }

        // First, fetch (branch name is validated above)
        let output = Command::new("git")
            .args(["fetch", "origin", branch])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git fetch: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git fetch failed: {}", stderr)));
        }

        // Checkout the branch (branch name is validated above)
        let output = Command::new("git")
            .args(["checkout", branch])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git checkout: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git checkout failed: {}", stderr)));
        }

        // Reset to origin/branch to ensure clean state
        let output = Command::new("git")
            .args(["reset", "--hard", &format!("origin/{}", branch)])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git reset: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git reset failed: {}", stderr)));
        }

        tracing::debug!(repo = ?repo_path, "Repository updated successfully");
        Ok(())
    }

    /// Check if a path is a valid git repository.
    pub fn is_git_repo(path: &Path) -> bool {
        path.join(".git").is_dir()
    }

    /// Get the current branch of a repository.
    pub async fn current_branch(repo_path: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git rev-parse: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git rev-parse failed: {}", stderr)));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_is_git_repo_true() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join(".git")).unwrap();
        assert!(GitOps::is_git_repo(temp.path()));
    }

    #[test]
    fn test_is_git_repo_false() {
        let temp = TempDir::new().unwrap();
        assert!(!GitOps::is_git_repo(temp.path()));
    }

    #[test]
    fn test_is_git_repo_nonexistent() {
        assert!(!GitOps::is_git_repo(Path::new("/nonexistent/path")));
    }
}

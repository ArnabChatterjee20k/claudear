//! Git operations for repository management.

use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Git operations for managing repositories.
pub struct GitOps;

/// Validate a ref (branch name, tag, etc.) to prevent command injection.
fn validate_ref(name: &str, label: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('-')
        || name.contains("..")
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@'))
    {
        return Err(Error::git(format!(
            "Invalid {} (contains disallowed characters): {}",
            label, name
        )));
    }
    Ok(())
}

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

    /// Ensure a repository's object store is current without checking out or resetting.
    ///
    /// If the repository doesn't exist locally, it will be cloned.
    /// If it exists, only `git fetch origin` is run — the working tree is untouched.
    pub async fn ensure_repo_fetched(repo_path: &Path, github_url: &str) -> Result<()> {
        if repo_path.exists() {
            Self::fetch_all(repo_path).await
        } else {
            Self::clone(github_url, repo_path).await
        }
    }

    /// Fetch all remote refs without touching the working tree.
    pub async fn fetch_all(repo_path: &Path) -> Result<()> {
        tracing::debug!(repo = ?repo_path, "Fetching all remote refs");

        let output = Command::new("git")
            .args(["fetch", "origin"])
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

        tracing::debug!(repo = ?repo_path, "Fetch completed");
        Ok(())
    }

    /// Fetch a specific branch from origin.
    pub async fn fetch_branch(repo_path: &Path, branch: &str) -> Result<()> {
        validate_ref(branch, "branch name")?;

        tracing::debug!(repo = ?repo_path, branch = %branch, "Fetching branch");

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
            return Err(Error::git(format!("git fetch branch failed: {}", stderr)));
        }

        Ok(())
    }

    /// Create a git worktree in detached HEAD state.
    ///
    /// If the worktree path already exists (e.g. from a crash), it is removed first.
    pub async fn create_worktree(
        repo_path: &Path,
        worktree_path: &Path,
        checkout_ref: &str,
    ) -> Result<()> {
        validate_ref(checkout_ref, "checkout ref")?;

        // Crash recovery: remove stale worktree
        if worktree_path.exists() {
            tracing::warn!(worktree = ?worktree_path, "Stale worktree found, removing");
            Self::remove_worktree(repo_path, worktree_path).await?;
        }

        // Create parent directory
        if let Some(parent) = worktree_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                Error::io(format!(
                    "Failed to create worktree parent directory {:?}: {}",
                    parent, e
                ))
            })?;
        }

        tracing::info!(
            repo = ?repo_path,
            worktree = ?worktree_path,
            checkout_ref = %checkout_ref,
            "Creating worktree"
        );

        let output = Command::new("git")
            .args(["worktree", "add", "--detach"])
            .arg(worktree_path)
            .arg(checkout_ref)
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git worktree add: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git(format!("git worktree add failed: {}", stderr)));
        }

        tracing::info!(worktree = ?worktree_path, "Worktree created");
        Ok(())
    }

    /// Remove a git worktree and clean up.
    pub async fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
        tracing::debug!(
            repo = ?repo_path,
            worktree = ?worktree_path,
            "Removing worktree"
        );

        // Try git worktree remove --force
        let output = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| Error::io(format!("Failed to execute git worktree remove: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(error = %stderr, "git worktree remove failed, falling back to rm");
        }

        // If the directory still exists, forcefully remove it
        if worktree_path.exists() {
            let has_worktrees_component = worktree_path
                .components()
                .any(|c| c.as_os_str().to_string_lossy().contains("-worktrees"));
            if !has_worktrees_component {
                return Err(Error::git(format!(
                    "Refusing to remove directory outside worktrees area: {:?}",
                    worktree_path
                )));
            }
            tokio::fs::remove_dir_all(worktree_path)
                .await
                .map_err(|e| {
                    Error::io(format!(
                        "Failed to remove worktree directory {:?}: {}",
                        worktree_path, e
                    ))
                })?;
        }

        // Prune stale worktree bookkeeping
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(repo_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        tracing::debug!(worktree = ?worktree_path, "Worktree removed");
        Ok(())
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
            .args(["clone", "--", url])
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
        validate_ref(branch, "branch name")?;

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

    /// Check if a path is a valid git repository (or worktree).
    pub fn is_git_repo(path: &Path) -> bool {
        let git_path = path.join(".git");
        git_path.is_dir() || git_path.is_file()
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

/// Compute the worktree path for a given issue.
///
/// Returns `work_dir/{short_repo_name}-worktrees/{issue_short_id}`.
pub fn worktree_path(work_dir: &Path, repo_name: &str, issue_short_id: &str) -> PathBuf {
    let short_name = repo_name.split('/').next_back().unwrap_or(repo_name);
    let sanitized_id = issue_short_id.replace(['/', '\\', '.'], "_");
    work_dir
        .join(format!("{}-worktrees", short_name))
        .join(sanitized_id)
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

    #[test]
    fn test_is_git_repo_file_not_dir() {
        // .git exists but as a file, not a directory (like a worktree)
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join(".git"), "gitdir: ../other/.git").unwrap();
        // Worktrees have .git as a file, so this should be true
        assert!(GitOps::is_git_repo(temp.path()));
    }

    #[test]
    fn test_worktree_path_simple() {
        let p = worktree_path(Path::new("/work"), "owner/repo", "ABC-123");
        assert_eq!(p, PathBuf::from("/work/repo-worktrees/ABC-123"));
    }

    #[test]
    fn test_worktree_path_no_owner() {
        let p = worktree_path(Path::new("/work"), "repo", "XYZ-1");
        assert_eq!(p, PathBuf::from("/work/repo-worktrees/XYZ-1"));
    }

    #[tokio::test]
    async fn test_clone_rejects_option_injection() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        // URL starting with '-' should be rejected (option injection)
        let result = GitOps::ensure_repo_at_path(&target, "--upload-pack=evil", "main").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("disallowed characters"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_clone_rejects_semicolon_in_url() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        let result =
            GitOps::ensure_repo_at_path(&target, "https://example.com;rm -rf /", "main").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("disallowed characters"));
    }

    #[tokio::test]
    async fn test_clone_rejects_pipe_in_url() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        let result = GitOps::ensure_repo_at_path(&target, "https://example.com|evil", "main").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clone_rejects_dollar_in_url() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        let result =
            GitOps::ensure_repo_at_path(&target, "https://example.com/$HOME", "main").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pull_rejects_option_injection_in_branch() {
        let temp = TempDir::new().unwrap();
        // Create a directory to trigger the pull path (repo_path.exists() = true)
        std::fs::create_dir_all(temp.path().join("repo")).unwrap();
        let target = temp.path().join("repo");

        let result =
            GitOps::ensure_repo_at_path(&target, "https://example.com/repo.git", "--evil-option")
                .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("disallowed characters"));
    }

    #[tokio::test]
    async fn test_pull_rejects_semicolon_in_branch() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("repo")).unwrap();
        let target = temp.path().join("repo");

        let result =
            GitOps::ensure_repo_at_path(&target, "https://example.com/repo.git", "main;evil").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_pull_rejects_dotdot_in_branch() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("repo")).unwrap();
        let target = temp.path().join("repo");

        let result =
            GitOps::ensure_repo_at_path(&target, "https://example.com/repo.git", "main..evil")
                .await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("disallowed characters"));
    }

    #[tokio::test]
    async fn test_clone_invalid_url_git_error() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        // Valid URL format but nonexistent - git clone should fail
        let result =
            GitOps::ensure_repo_at_path(&target, "https://nonexistent.invalid/repo.git", "main")
                .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_current_branch_non_git_dir() {
        let temp = TempDir::new().unwrap();
        let result = GitOps::current_branch(temp.path()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fetch_branch_rejects_injection() {
        let temp = TempDir::new().unwrap();
        let result = GitOps::fetch_branch(temp.path(), "--evil").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("disallowed characters"));
    }

    #[tokio::test]
    async fn test_create_worktree_rejects_injection() {
        let temp = TempDir::new().unwrap();
        let wt = temp.path().join("wt");
        let result = GitOps::create_worktree(temp.path(), &wt, "--evil-ref").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("disallowed characters"));
    }

    #[tokio::test]
    async fn test_ensure_repo_fetched_clones_when_missing() {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("repo");

        // Nonexistent URL should fail at clone, not panic
        let result =
            GitOps::ensure_repo_fetched(&target, "https://nonexistent.invalid/repo.git").await;
        assert!(result.is_err());
    }
}

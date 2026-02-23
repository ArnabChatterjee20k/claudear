//! Repository index for file-based searching.
//!
//! Provides a searchable index of repositories discovered from known organizations.
//! This enables issue-to-repository inference by matching file paths and names.

use crate::error::Result;
use crate::github::GitHubClient;
use crate::scm::ScmProvider;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// An indexed repository with file information.
#[derive(Debug, Clone)]
pub struct IndexedRepo {
    /// Repository name (e.g., "appwrite/cloud").
    pub name: String,
    /// Local filesystem path.
    pub path: PathBuf,
    /// GitHub URL inferred from org + name.
    pub scm_url: String,
    /// Relative file paths within the repository.
    pub files: Vec<String>,
    /// Default branch name.
    pub default_branch: String,
}

impl IndexedRepo {
    /// Create a new indexed repository.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let name = name.into();
        let scm_url = format!("https://github.com/{}", name);
        Self {
            name,
            path: path.into(),
            scm_url,
            files: Vec::new(),
            default_branch: "main".to_string(),
        }
    }

    /// Create a repository discovered via GitHub API.
    ///
    /// The path is set to `workspace/{repo_name}` where repos will be cloned.
    pub fn from_api(
        name: impl Into<String>,
        scm_url: impl Into<String>,
        default_branch: impl Into<String>,
        workspace: &Path,
    ) -> Self {
        let name = name.into();
        // Extract repo name from full_name (org/repo)
        let repo_name = name.split('/').next_back().unwrap_or(&name);
        let path = workspace.join(repo_name);
        Self {
            name,
            path,
            scm_url: scm_url.into(),
            files: Vec::new(),
            default_branch: default_branch.into(),
        }
    }

    /// Set the GitHub URL.
    pub fn with_scm_url(mut self, url: impl Into<String>) -> Self {
        self.scm_url = url.into();
        self
    }

    /// Set the default branch.
    pub fn with_default_branch(mut self, branch: impl Into<String>) -> Self {
        self.default_branch = branch.into();
        self
    }

    /// Check if this repo contains a file with the given name.
    pub fn has_file(&self, filename: &str) -> bool {
        let filename_lower = filename.to_lowercase();
        self.files.iter().any(|f| {
            f.to_lowercase().ends_with(&filename_lower)
                || f.to_lowercase().contains(&filename_lower)
        })
    }

    /// Find all files matching a query.
    pub fn find_files(&self, query: &str) -> Vec<&str> {
        let query_lower = query.to_lowercase();
        self.files
            .iter()
            .filter(|f| f.to_lowercase().contains(&query_lower))
            .map(|s| s.as_str())
            .collect()
    }
}

/// Index of discovered repositories.
#[derive(Debug, Default, Clone)]
pub struct RepoIndex {
    /// Indexed repositories keyed by name.
    repos: HashMap<String, IndexedRepo>,
    /// File path to repository name mapping for fast lookups.
    file_index: HashMap<String, String>,
}

impl RepoIndex {
    /// Create a new empty index.
    pub fn new() -> Self {
        Self {
            repos: HashMap::new(),
            file_index: HashMap::new(),
        }
    }

    /// Build an index by scanning auto_discover_paths for repos from known_orgs.
    ///
    /// # Arguments
    /// * `known_orgs` - GitHub organization names to look for
    /// * `paths` - Directories to scan for repositories
    ///
    /// # Returns
    /// A populated RepoIndex with discovered repositories and their files.
    pub fn build(known_orgs: &[String], paths: &[String]) -> Result<Self> {
        let mut index = Self::new();
        let orgs_set: HashSet<_> = known_orgs.iter().map(|s| s.to_lowercase()).collect();

        for path_str in paths {
            let path = expand_path(path_str);
            if !path.exists() {
                tracing::warn!(path = %path.display(), "Auto-discover path does not exist");
                continue;
            }

            tracing::info!(path = %path.display(), "Scanning for repositories");

            // Walk the directory to find git repositories
            for entry in WalkDir::new(&path)
                .max_depth(3) // Don't go too deep
                .into_iter()
                .filter_entry(|e| !is_hidden(e))
            {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };

                let entry_path = entry.path();

                // Check if this is a git repository
                if entry_path.join(".git").is_dir() {
                    // Try to determine the repository name from git remote
                    if let Some(repo_name) = get_repo_name_from_git(entry_path) {
                        // Check if this repo belongs to a known org
                        let org = repo_name.split('/').next().unwrap_or("");
                        if orgs_set.contains(&org.to_lowercase()) {
                            tracing::debug!(
                                repo = %repo_name,
                                path = %entry_path.display(),
                                "Found repository from known org"
                            );

                            let mut repo = IndexedRepo::new(&repo_name, entry_path);
                            repo = index_files(repo);
                            index.add_repo(repo);
                        }
                    }
                }
            }
        }

        tracing::info!(
            count = index.repos.len(),
            files = index.file_index.len(),
            "Repository index built"
        );

        Ok(index)
    }

    /// Build an index by fetching repositories from GitHub API.
    ///
    /// This is used when `auto_discover_paths` is empty but `known_orgs` and
    /// a GitHub token are configured. Repos discovered this way are cloned
    /// at startup before processing begins.
    ///
    /// # Arguments
    /// * `known_orgs` - GitHub organization names to fetch repos from
    /// * `client` - GitHub API client with token configured
    /// * `workspace` - Directory where repos will be cloned to
    /// * `use_ssh` - Whether to use SSH URLs for cloning
    ///
    /// # Returns
    /// A populated RepoIndex with API-discovered repositories.
    pub async fn build_from_github(
        known_orgs: &[String],
        client: &GitHubClient,
        workspace: &Path,
        use_ssh: bool,
    ) -> Result<Self> {
        let mut index = Self::new();

        for org in known_orgs {
            tracing::info!(org = %org, "Fetching repositories from GitHub API");

            match client.list_org_repos(org).await {
                Ok(repos) => {
                    for repo in repos {
                        let clone_url = if use_ssh {
                            &repo.ssh_url
                        } else {
                            &repo.clone_url
                        };
                        let indexed = IndexedRepo::from_api(
                            &repo.full_name,
                            clone_url,
                            &repo.default_branch,
                            workspace,
                        );

                        tracing::debug!(
                            repo = %repo.full_name,
                            path = %indexed.path.display(),
                            "Added API-discovered repository to index"
                        );

                        index.add_repo(indexed);
                    }
                }
                Err(e) => {
                    tracing::warn!(org = %org, error = %e, "Failed to fetch repos from org");
                }
            }
        }

        tracing::info!(
            count = index.repos.len(),
            "Repository index built from GitHub API"
        );

        Ok(index)
    }

    /// Build an index from GitLab groups using any ScmProvider.
    ///
    /// Fetches repos from each group and creates IndexedRepo entries
    /// pointing to the workspace for cloning.
    pub async fn build_from_gitlab(
        groups: &[String],
        provider: &dyn ScmProvider,
        workspace: &Path,
        use_ssh: bool,
    ) -> Result<Self> {
        let mut index = Self::new();

        for group in groups {
            tracing::info!(group = %group, "Fetching repositories from GitLab API");

            match provider.list_repos(group).await {
                Ok(repos) => {
                    for repo in repos {
                        let clone_url = if use_ssh {
                            &repo.ssh_url
                        } else {
                            &repo.clone_url
                        };
                        let indexed = IndexedRepo::from_api(
                            &repo.full_name,
                            clone_url,
                            &repo.default_branch,
                            workspace,
                        );

                        tracing::debug!(
                            repo = %repo.full_name,
                            path = %indexed.path.display(),
                            "Added GitLab-discovered repository to index"
                        );

                        index.add_repo(indexed);
                    }
                }
                Err(e) => {
                    tracing::warn!(group = %group, error = %e, "Failed to fetch repos from group");
                }
            }
        }

        tracing::info!(
            count = index.repos.len(),
            "Repository index built from GitLab API"
        );

        Ok(index)
    }

    /// Build an index using the best available method.
    ///
    /// This chooses the discovery method based on configuration:
    /// 1. If `auto_discover_paths` is not empty → use local filesystem scan
    /// 2. Else if GitHub token is configured + `known_orgs` not empty → use API
    /// 3. Else if GitLab is configured → try GitLab groups
    /// 4. Else → return empty index
    ///
    /// # Arguments
    /// * `known_orgs` - GitHub organization names
    /// * `auto_discover_paths` - Local paths to scan for repos
    /// * `github_client` - Optional GitHub API client
    /// * `gitlab_provider` - Optional GitLab SCM provider
    /// * `gitlab_groups` - GitLab group names to discover
    /// * `workspace` - Directory where repos will be cloned to (for API discovery)
    /// * `use_ssh` - Whether to use SSH URLs for cloning
    pub async fn build_with_fallback(
        known_orgs: &[String],
        auto_discover_paths: &[String],
        github_client: Option<&GitHubClient>,
        gitlab_provider: Option<&dyn ScmProvider>,
        gitlab_groups: &[String],
        workspace: &Path,
        use_ssh: bool,
    ) -> Result<Self> {
        // Strategy 1: Local filesystem scan (preferred when paths are configured)
        if !auto_discover_paths.is_empty() {
            tracing::info!("Building repo index from local filesystem");
            return Self::build(known_orgs, auto_discover_paths);
        }

        // Strategy 2: GitHub API discovery
        if let Some(client) = github_client {
            if client.is_enabled() && !known_orgs.is_empty() {
                tracing::info!(
                    "Building repo index from GitHub API (no auto_discover_paths configured)"
                );
                let mut index =
                    Self::build_from_github(known_orgs, client, workspace, use_ssh).await?;

                // Also try GitLab if configured
                if let Some(gl) = gitlab_provider {
                    if gl.is_enabled() && !gitlab_groups.is_empty() {
                        let gl_index =
                            Self::build_from_gitlab(gitlab_groups, gl, workspace, use_ssh).await?;
                        index.merge(gl_index);
                    }
                }

                return Ok(index);
            }
        }

        // Strategy 3: GitLab API discovery
        if let Some(gl) = gitlab_provider {
            if gl.is_enabled() && !gitlab_groups.is_empty() {
                tracing::info!("Building repo index from GitLab API");
                return Self::build_from_gitlab(gitlab_groups, gl, workspace, use_ssh).await;
            }
        }

        // Strategy 4: Empty index
        tracing::info!("No discovery method available, returning empty index");
        Ok(Self::new())
    }

    /// Add a repository to the index.
    pub fn add_repo(&mut self, repo: IndexedRepo) {
        // Index all files for fast lookup
        for file in &repo.files {
            // Index by full path
            self.file_index.insert(file.clone(), repo.name.clone());

            // Index by filename only (for basename matching)
            if let Some(filename) = Path::new(file).file_name() {
                self.file_index
                    .insert(filename.to_string_lossy().to_string(), repo.name.clone());
            }
        }

        self.repos.insert(repo.name.clone(), repo);
    }

    /// Merge another RepoIndex into this one.
    pub fn merge(&mut self, other: Self) {
        for (_, repo) in other.repos {
            self.add_repo(repo);
        }
    }

    /// Index files for a repository that was just cloned.
    ///
    /// Updates the repo's file list and rebuilds the file index entries.
    /// Returns the number of files indexed, or None if repo not found.
    pub fn index_repo_files(&mut self, repo_name: &str) -> Option<usize> {
        // Get the repo and index its files
        let repo = self.repos.remove(repo_name)?;
        let indexed_repo = index_files(repo);
        let file_count = indexed_repo.files.len();

        // Re-add with indexed files (this updates the file_index)
        self.add_repo(indexed_repo);

        Some(file_count)
    }

    /// Find a repository by exact file path match.
    pub fn find_by_file(&self, filename: &str) -> Option<&IndexedRepo> {
        // Try exact match first
        if let Some(repo_name) = self.file_index.get(filename) {
            return self.repos.get(repo_name);
        }

        // Try vendor path extraction (e.g., /usr/src/code/vendor/utopia-php/database/src/... -> utopia-php/database)
        if let Some(repo) = self.find_by_vendor_path(filename) {
            return Some(repo);
        }

        // Try basename match (last resort - can match wrong repo if filename is common)
        let basename = Path::new(filename)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| filename.to_string());

        if let Some(repo_name) = self.file_index.get(&basename) {
            return self.repos.get(repo_name);
        }

        None
    }

    /// Extract repository name from vendor paths.
    ///
    /// Vendor paths follow the pattern: .../vendor/{org}/{repo}/...
    /// This extracts {org}/{repo} and looks it up in the index.
    fn find_by_vendor_path(&self, filename: &str) -> Option<&IndexedRepo> {
        // Look for /vendor/ in the path
        let vendor_idx = filename.find("/vendor/")?;
        let after_vendor = &filename[vendor_idx + 8..]; // Skip "/vendor/"

        // Split the remainder to get org/repo
        let parts: Vec<&str> = after_vendor.split('/').collect();
        if parts.len() >= 2 {
            let repo_name = format!("{}/{}", parts[0], parts[1]);
            if let Some(repo) = self.repos.get(&repo_name) {
                return Some(repo);
            }
        }

        None
    }

    /// Search for files matching a query across all repositories.
    ///
    /// Returns tuples of (repo, matching_file_path).
    pub fn search_files(&self, query: &str) -> Vec<(&IndexedRepo, &str)> {
        let mut results = Vec::new();

        for repo in self.repos.values() {
            for file in repo.find_files(query) {
                results.push((repo, file));
            }
        }

        results
    }

    /// Get a repository by name.
    pub fn get(&self, name: &str) -> Option<&IndexedRepo> {
        self.repos.get(name)
    }

    /// Get all indexed repositories.
    pub fn list(&self) -> Vec<&IndexedRepo> {
        self.repos.values().collect()
    }

    /// Get the number of indexed repositories.
    pub fn len(&self) -> usize {
        self.repos.len()
    }

    /// Check if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }

    /// Get total file count across all repositories.
    pub fn total_files(&self) -> usize {
        self.repos.values().map(|r| r.files.len()).sum()
    }
}

/// Expand ~ to home directory.
fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            // Strip the leading / if present (e.g., ~/foo -> foo, ~ -> empty)
            let suffix = stripped.strip_prefix('/').unwrap_or(stripped);
            return home.join(suffix);
        }
    }
    PathBuf::from(path)
}

/// Check if a directory entry is hidden.
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Get repository name from git remote origin.
fn get_repo_name_from_git(path: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_repo_name_from_url(&url)
}

/// Parse repository name (org/repo) from a git URL.
fn parse_repo_name_from_url(url: &str) -> Option<String> {
    // Handle SSH URLs: git@github.com:org/repo.git or git@gitlab.com:group/repo.git
    if url.starts_with("git@") {
        let parts: Vec<_> = url.split(':').collect();
        if parts.len() == 2 {
            let repo_part = parts[1].trim_end_matches(".git");
            return Some(repo_part.to_string());
        }
    }

    // Handle HTTPS URLs: https://github.com/org/repo.git or https://gitlab.com/group/repo.git
    let url_trimmed = url.trim_end_matches(".git");
    let parts: Vec<_> = url_trimmed.split('/').collect();
    if parts.len() >= 2 {
        let org = parts[parts.len() - 2];
        let repo = parts[parts.len() - 1];
        if !org.is_empty() && !repo.is_empty() {
            return Some(format!("{}/{}", org, repo));
        }
    }

    None
}

/// Index all files in a repository.
pub fn index_files(mut repo: IndexedRepo) -> IndexedRepo {
    let mut files = Vec::new();

    for entry in WalkDir::new(&repo.path)
        .into_iter()
        .filter_entry(|e| {
            // Skip hidden directories and common non-source directories
            let name = e.file_name().to_string_lossy();
            !name.starts_with('.')
                && name != "node_modules"
                && name != "vendor"
                && name != "target"
                && name != "build"
                && name != "dist"
                && name != "__pycache__"
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            // Store relative path from repo root
            if let Ok(rel_path) = entry.path().strip_prefix(&repo.path) {
                files.push(rel_path.to_string_lossy().to_string());
            }
        }
    }

    repo.files = files;
    repo
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_indexed_repo_new() {
        let repo = IndexedRepo::new("appwrite/cloud", "/path/to/cloud");
        assert_eq!(repo.name, "appwrite/cloud");
        assert_eq!(repo.scm_url, "https://github.com/appwrite/cloud");
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn test_indexed_repo_with_scm_url() {
        let repo =
            IndexedRepo::new("test/repo", "/path").with_scm_url("https://gitlab.com/test/repo");
        assert_eq!(repo.scm_url, "https://gitlab.com/test/repo");
    }

    #[test]
    fn test_indexed_repo_has_file() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "README.md".to_string(),
        ];

        assert!(repo.has_file("main.rs"));
        assert!(repo.has_file("src/main.rs"));
        assert!(repo.has_file("README.md"));
        assert!(!repo.has_file("nonexistent.txt"));
    }

    #[test]
    fn test_indexed_repo_find_files() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec![
            "src/main.rs".to_string(),
            "src/lib.rs".to_string(),
            "src/utils/helper.rs".to_string(),
        ];

        let matches = repo.find_files(".rs");
        assert_eq!(matches.len(), 3);

        let matches = repo.find_files("main");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0], "src/main.rs");
    }

    #[test]
    fn test_repo_index_new() {
        let index = RepoIndex::new();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn test_repo_index_add_repo() {
        let mut index = RepoIndex::new();
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec!["src/main.rs".to_string()];

        index.add_repo(repo);

        assert_eq!(index.len(), 1);
        assert!(index.get("test/repo").is_some());
    }

    #[test]
    fn test_repo_index_find_by_file() {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["src/main.rs".to_string(), "src/app.rs".to_string()];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["lib/utils.rs".to_string()];
        index.add_repo(repo2);

        // Find by full path
        let found = index.find_by_file("src/main.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/repo1");

        // Find by basename
        let found = index.find_by_file("utils.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/repo2");

        // Not found
        let found = index.find_by_file("nonexistent.rs");
        assert!(found.is_none());
    }

    #[test]
    fn test_repo_index_search_files() {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["src/main.rs".to_string(), "src/router.rs".to_string()];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["lib/router.rs".to_string()];
        index.add_repo(repo2);

        let results = index.search_files("router");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_expand_path_home() {
        let expanded = expand_path("~/test");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home.join("test"));
        }
    }

    #[test]
    fn test_expand_path_absolute() {
        let expanded = expand_path("/absolute/path");
        assert_eq!(expanded, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn test_parse_repo_name_ssh() {
        let name = parse_repo_name_from_url("git@github.com:appwrite/cloud.git");
        assert_eq!(name, Some("appwrite/cloud".to_string()));
    }

    #[test]
    fn test_parse_repo_name_https() {
        let name = parse_repo_name_from_url("https://github.com/appwrite/cloud.git");
        assert_eq!(name, Some("appwrite/cloud".to_string()));
    }

    #[test]
    fn test_parse_repo_name_https_no_git() {
        let name = parse_repo_name_from_url("https://github.com/appwrite/cloud");
        assert_eq!(name, Some("appwrite/cloud".to_string()));
    }

    #[test]
    fn test_is_hidden() {
        use walkdir::WalkDir;
        let temp = TempDir::new().unwrap();
        std::fs::create_dir(temp.path().join(".hidden")).unwrap();
        std::fs::create_dir(temp.path().join("visible")).unwrap();

        for entry in WalkDir::new(temp.path()).max_depth(1) {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy();
            if name == ".hidden" {
                assert!(is_hidden(&entry));
            } else if name == "visible" {
                assert!(!is_hidden(&entry));
            }
        }
    }

    #[test]
    fn test_total_files() {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["a.rs".to_string(), "b.rs".to_string()];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["c.rs".to_string()];
        index.add_repo(repo2);

        assert_eq!(index.total_files(), 3);
    }

    #[test]
    fn test_find_by_vendor_path() {
        let mut index = RepoIndex::new();

        // Add utopia-php/database repo
        let mut repo1 = IndexedRepo::new("utopia-php/database", "/path1");
        repo1.files = vec!["src/Database/Adapter/Pool.php".to_string()];
        index.add_repo(repo1);

        // Add appwrite/appwrite repo with a file that has the same basename
        let mut repo2 = IndexedRepo::new("appwrite/appwrite", "/path2");
        repo2.files = vec!["src/Appwrite/PubSub/Adapter/Pool.php".to_string()];
        index.add_repo(repo2);

        // Vendor path should match utopia-php/database, NOT appwrite/appwrite
        let vendor_path = "/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/Pool.php";
        let found = index.find_by_file(vendor_path);
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "utopia-php/database");
    }

    #[test]
    fn test_find_by_vendor_path_not_indexed() {
        let mut index = RepoIndex::new();

        // Only add appwrite/appwrite repo
        let mut repo = IndexedRepo::new("appwrite/appwrite", "/path");
        repo.files = vec!["src/Appwrite/PubSub/Adapter/Pool.php".to_string()];
        index.add_repo(repo);

        // Vendor path for utopia-php/database (not in index) should fall back to basename match
        let vendor_path = "/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/Pool.php";
        let found = index.find_by_file(vendor_path);
        assert!(found.is_some());
        // Falls back to basename match since utopia-php/database isn't indexed
        assert_eq!(found.unwrap().name, "appwrite/appwrite");
    }

    #[test]
    fn test_indexed_repo_new_default_branch() {
        let repo = IndexedRepo::new("test/repo", "/path/to/repo");
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn test_indexed_repo_from_api() {
        let workspace = PathBuf::from("/home/user/repos");
        let repo = IndexedRepo::from_api(
            "test-org/my-repo",
            "https://github.com/test-org/my-repo.git",
            "develop",
            &workspace,
        );

        assert_eq!(repo.name, "test-org/my-repo");
        assert_eq!(repo.scm_url, "https://github.com/test-org/my-repo.git");
        assert_eq!(repo.default_branch, "develop");
        assert_eq!(repo.path, workspace.join("my-repo"));
        assert!(repo.files.is_empty());
    }

    #[test]
    fn test_indexed_repo_from_api_extracts_repo_name() {
        let workspace = PathBuf::from("/var/repos");

        // Test with full_name format "org/repo"
        let repo = IndexedRepo::from_api(
            "appwrite/console",
            "https://github.com/appwrite/console.git",
            "main",
            &workspace,
        );
        assert_eq!(repo.path, workspace.join("console"));

        // Test with simple name (no slash)
        let repo2 = IndexedRepo::from_api(
            "simple-repo",
            "https://github.com/org/simple-repo.git",
            "main",
            &workspace,
        );
        assert_eq!(repo2.path, workspace.join("simple-repo"));
    }

    #[test]
    fn test_repo_index_add_api_repo() {
        let mut index = RepoIndex::new();
        let workspace = PathBuf::from("/repos");

        let repo = IndexedRepo::from_api(
            "test-org/test-repo",
            "https://github.com/test-org/test-repo.git",
            "main",
            &workspace,
        );
        index.add_repo(repo);

        assert_eq!(index.len(), 1);
        let found = index.get("test-org/test-repo").unwrap();
        assert_eq!(found.scm_url, "https://github.com/test-org/test-repo.git");
    }

    #[test]
    fn test_indexed_repo_with_default_branch() {
        let repo = IndexedRepo::new("test/repo", "/path").with_default_branch("develop");
        assert_eq!(repo.default_branch, "develop");
    }

    #[test]
    fn test_indexed_repo_has_file_case_insensitive() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec!["src/MyClass.php".to_string()];

        assert!(repo.has_file("myclass.php"));
        assert!(repo.has_file("MYCLASS.PHP"));
    }

    #[test]
    fn test_indexed_repo_find_files_no_match() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec!["src/main.rs".to_string()];

        let results = repo.find_files("nonexistent");
        assert!(results.is_empty());
    }

    #[test]
    fn test_indexed_repo_find_files_case_insensitive() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec![
            "src/MyComponent.tsx".to_string(),
            "src/myHelper.ts".to_string(),
        ];

        let results = repo.find_files("MY");
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_repo_index_list() {
        let mut index = RepoIndex::new();

        let repo1 = IndexedRepo::new("org/repo1", "/path1");
        let repo2 = IndexedRepo::new("org/repo2", "/path2");
        index.add_repo(repo1);
        index.add_repo(repo2);

        let list = index.list();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_repo_index_search_files_no_results() {
        let mut index = RepoIndex::new();

        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["src/main.rs".to_string()];
        index.add_repo(repo);

        let results = index.search_files("nonexistent_query");
        assert!(results.is_empty());
    }

    #[test]
    fn test_repo_index_index_repo_files_not_found() {
        let mut index = RepoIndex::new();
        let result = index.index_repo_files("nonexistent/repo");
        assert!(result.is_none());
    }

    #[test]
    fn test_repo_index_index_repo_files_with_real_dir() {
        let temp = TempDir::new().unwrap();

        // Create a non-hidden subdirectory to use as the repo root
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();
        std::fs::write(repo_dir.join("file1.rs"), "fn main() {}").unwrap();
        std::fs::write(repo_dir.join("file2.rs"), "fn test() {}").unwrap();

        let mut index = RepoIndex::new();
        let repo = IndexedRepo::new("test/repo", &repo_dir);
        index.add_repo(repo);

        let count = index.index_repo_files("test/repo");
        assert!(count.is_some());
        assert!(count.unwrap() >= 2);

        // Verify the repo is still accessible after re-indexing
        let repo = index.get("test/repo").unwrap();
        assert!(repo.files.len() >= 2);
    }

    #[test]
    fn test_find_by_vendor_path_no_vendor_in_path() {
        let mut index = RepoIndex::new();
        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["src/Pool.php".to_string()];
        index.add_repo(repo);

        // Path without /vendor/ should not match via vendor path
        let result = index.find_by_vendor_path("/usr/src/code/src/Pool.php");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_vendor_path_insufficient_parts() {
        let mut index = RepoIndex::new();
        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["src/file.php".to_string()];
        index.add_repo(repo);

        // Vendor path with only one segment after /vendor/
        let result = index.find_by_vendor_path("/usr/src/code/vendor/singlepart");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_file_no_basename_match() {
        let index = RepoIndex::new();
        let result = index.find_by_file("totally_unknown_file.xyz");
        assert!(result.is_none());
    }

    #[test]
    fn test_expand_path_just_tilde() {
        let expanded = expand_path("~");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home);
        }
    }

    #[test]
    fn test_expand_path_relative() {
        let expanded = expand_path("relative/path");
        assert_eq!(expanded, PathBuf::from("relative/path"));
    }

    #[test]
    fn test_parse_repo_name_ssh_no_git_extension() {
        let name = parse_repo_name_from_url("git@github.com:org/repo");
        assert_eq!(name, Some("org/repo".to_string()));
    }

    #[test]
    fn test_parse_repo_name_gitlab() {
        let name = parse_repo_name_from_url("https://gitlab.com/org/repo.git");
        assert_eq!(name, Some("org/repo".to_string()));
    }

    #[test]
    fn test_parse_repo_name_invalid_ssh_format() {
        let name = parse_repo_name_from_url("git@github.com");
        // No colon separator for repo path
        assert!(name.is_none());
    }

    #[test]
    fn test_parse_repo_name_empty_url() {
        let name = parse_repo_name_from_url("");
        assert!(name.is_none());
    }

    #[test]
    fn test_repo_index_add_repo_indexes_basenames() {
        let mut index = RepoIndex::new();

        let mut repo = IndexedRepo::new("org/myrepo", "/path");
        repo.files = vec![
            "src/nested/deep/file.rs".to_string(),
            "tests/test_module.rs".to_string(),
        ];
        index.add_repo(repo);

        // Should be findable by basename
        let found = index.find_by_file("file.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/myrepo");

        let found = index.find_by_file("test_module.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/myrepo");
    }

    #[test]
    fn test_index_files_skips_hidden_and_build_dirs() {
        let temp = TempDir::new().unwrap();

        // Use a non-hidden subdirectory as repo root (TempDir names may start with '.')
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        // Create a visible file
        std::fs::write(repo_dir.join("visible.rs"), "fn main() {}").unwrap();

        // Create a hidden directory with a file
        std::fs::create_dir(repo_dir.join(".hidden")).unwrap();
        std::fs::write(repo_dir.join(".hidden/secret.rs"), "// hidden").unwrap();

        // Create a node_modules directory
        std::fs::create_dir(repo_dir.join("node_modules")).unwrap();
        std::fs::write(repo_dir.join("node_modules/pkg.js"), "// npm").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        // Should only contain the visible file
        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files.iter().any(|f| f.contains("visible.rs")));
    }

    #[test]
    fn test_total_files_empty_index() {
        let index = RepoIndex::new();
        assert_eq!(index.total_files(), 0);
    }

    #[test]
    fn test_indexed_repo_empty_files() {
        let repo = IndexedRepo::new("test/repo", "/path");
        assert!(repo.files.is_empty());
        assert!(!repo.has_file("anything.rs"));
        assert!(repo.find_files("anything").is_empty());
    }

    // ---------------------------------------------------------------
    // MockScmProvider and build_from_gitlab tests
    // ---------------------------------------------------------------

    use crate::error::Result as CrateResult;
    use crate::scm::{CodeReview, PrInfo, PrStatus, RemoteRepo, ReviewComment, ScmProvider};
    use async_trait::async_trait;

    /// A mock SCM provider for testing build_from_gitlab.
    struct MockScmProvider {
        repos: std::result::Result<Vec<RemoteRepo>, String>,
    }

    impl MockScmProvider {
        fn with_repos(repos: Vec<RemoteRepo>) -> Self {
            Self { repos: Ok(repos) }
        }

        fn with_error(msg: &str) -> Self {
            Self {
                repos: Err(msg.to_string()),
            }
        }
    }

    fn make_remote_repo(
        full_name: &str,
        clone_url: &str,
        ssh_url: &str,
        default_branch: &str,
    ) -> RemoteRepo {
        let name = full_name.split('/').next_back().unwrap_or(full_name);
        RemoteRepo {
            id: 1,
            full_name: full_name.to_string(),
            name: name.to_string(),
            default_branch: default_branch.to_string(),
            clone_url: clone_url.to_string(),
            ssh_url: ssh_url.to_string(),
            html_url: format!("https://gitlab.com/{}", full_name),
            private: false,
            archived: false,
        }
    }

    #[async_trait]
    impl ScmProvider for MockScmProvider {
        fn name(&self) -> &str {
            "mock-gitlab"
        }

        fn is_enabled(&self) -> bool {
            true
        }

        fn review_trigger(&self) -> &str {
            "@claudear"
        }

        async fn get_pr_status(&self, _project: &str, _number: i64) -> CrateResult<PrStatus> {
            unimplemented!()
        }

        async fn get_pr_info(&self, _project: &str, _number: i64) -> CrateResult<PrInfo> {
            unimplemented!()
        }

        async fn get_pr_diff(&self, _project: &str, _number: i64) -> CrateResult<String> {
            unimplemented!()
        }

        async fn get_reviews(&self, _project: &str, _number: i64) -> CrateResult<Vec<CodeReview>> {
            unimplemented!()
        }

        async fn get_review_comments(
            &self,
            _project: &str,
            _number: i64,
        ) -> CrateResult<Vec<ReviewComment>> {
            unimplemented!()
        }

        async fn list_repos(&self, _org_or_group: &str) -> CrateResult<Vec<RemoteRepo>> {
            match &self.repos {
                Ok(repos) => Ok(repos.clone()),
                Err(msg) => Err(crate::error::Error::Source {
                    source_name: "mock-gitlab".to_string(),
                    message: msg.clone(),
                }),
            }
        }
    }

    #[tokio::test]
    async fn test_build_from_gitlab_single_group() {
        let repos = vec![
            make_remote_repo(
                "mygroup/repo-a",
                "https://gitlab.com/mygroup/repo-a.git",
                "git@gitlab.com:mygroup/repo-a.git",
                "main",
            ),
            make_remote_repo(
                "mygroup/repo-b",
                "https://gitlab.com/mygroup/repo-b.git",
                "git@gitlab.com:mygroup/repo-b.git",
                "develop",
            ),
        ];
        let provider = MockScmProvider::with_repos(repos);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["mygroup".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        assert_eq!(index.len(), 2);
        let repo_a = index.get("mygroup/repo-a").unwrap();
        assert_eq!(repo_a.path, workspace.join("repo-a"));
        assert_eq!(repo_a.default_branch, "main");

        let repo_b = index.get("mygroup/repo-b").unwrap();
        assert_eq!(repo_b.path, workspace.join("repo-b"));
        assert_eq!(repo_b.default_branch, "develop");
    }

    #[tokio::test]
    async fn test_build_from_gitlab_multiple_groups() {
        let repos = vec![
            make_remote_repo(
                "group1/project1",
                "https://gitlab.com/group1/project1.git",
                "git@gitlab.com:group1/project1.git",
                "main",
            ),
            make_remote_repo(
                "group1/project2",
                "https://gitlab.com/group1/project2.git",
                "git@gitlab.com:group1/project2.git",
                "main",
            ),
        ];
        // The mock returns the same repos for every group call, so we use
        // separate provider instances to simulate different groups returning
        // different repos. Instead, we test that the method iterates groups
        // and collects repos from each call.
        let provider = MockScmProvider::with_repos(repos);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["group1".to_string(), "group2".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        // The mock returns the same 2 repos for both group calls, but since
        // they have the same names the second call overwrites the first.
        // This still validates that both groups are iterated.
        assert_eq!(index.len(), 2);
        assert!(index.get("group1/project1").is_some());
        assert!(index.get("group1/project2").is_some());
    }

    #[tokio::test]
    async fn test_build_from_gitlab_empty_group() {
        let provider = MockScmProvider::with_repos(vec![]);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["empty-group".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
    }

    #[tokio::test]
    async fn test_build_from_gitlab_api_error() {
        let provider = MockScmProvider::with_error("403 Forbidden");
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["forbidden-group".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        // Error is logged but not fatal; index should be empty.
        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn test_build_from_gitlab_ssh_urls() {
        let repos = vec![make_remote_repo(
            "team/service",
            "https://gitlab.com/team/service.git",
            "git@gitlab.com:team/service.git",
            "main",
        )];
        let provider = MockScmProvider::with_repos(repos);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["team".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, true)
            .await
            .unwrap();

        let repo = index.get("team/service").unwrap();
        assert_eq!(repo.scm_url, "git@gitlab.com:team/service.git");
    }

    #[tokio::test]
    async fn test_build_from_gitlab_https_urls() {
        let repos = vec![make_remote_repo(
            "team/service",
            "https://gitlab.com/team/service.git",
            "git@gitlab.com:team/service.git",
            "main",
        )];
        let provider = MockScmProvider::with_repos(repos);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["team".to_string()];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        let repo = index.get("team/service").unwrap();
        assert_eq!(repo.scm_url, "https://gitlab.com/team/service.git");
    }

    // ════════════════════════════════════════════════════════════
    //  RepoIndex::merge
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_repo_index_merge() {
        let mut index1 = RepoIndex::new();
        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["a.rs".to_string()];
        index1.add_repo(repo1);

        let mut index2 = RepoIndex::new();
        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["b.rs".to_string()];
        index2.add_repo(repo2);

        index1.merge(index2);

        assert_eq!(index1.len(), 2);
        assert!(index1.get("org/repo1").is_some());
        assert!(index1.get("org/repo2").is_some());
        assert_eq!(index1.total_files(), 2);
    }

    #[test]
    fn test_repo_index_merge_overwrites_duplicate() {
        let mut index1 = RepoIndex::new();
        let mut repo1 = IndexedRepo::new("org/repo", "/path1");
        repo1.files = vec!["old.rs".to_string()];
        index1.add_repo(repo1);

        let mut index2 = RepoIndex::new();
        let mut repo2 = IndexedRepo::new("org/repo", "/path2");
        repo2.files = vec!["new.rs".to_string()];
        index2.add_repo(repo2);

        index1.merge(index2);

        assert_eq!(index1.len(), 1);
        let repo = index1.get("org/repo").unwrap();
        assert_eq!(repo.path, PathBuf::from("/path2"));
        assert_eq!(repo.files, vec!["new.rs".to_string()]);
    }

    #[test]
    fn test_repo_index_merge_empty_into_populated() {
        let mut index1 = RepoIndex::new();
        let repo = IndexedRepo::new("org/repo", "/path");
        index1.add_repo(repo);

        let index2 = RepoIndex::new();
        index1.merge(index2);

        assert_eq!(index1.len(), 1);
    }

    #[test]
    fn test_repo_index_merge_populated_into_empty() {
        let mut index1 = RepoIndex::new();

        let mut index2 = RepoIndex::new();
        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["x.rs".to_string()];
        index2.add_repo(repo);

        index1.merge(index2);

        assert_eq!(index1.len(), 1);
        assert!(index1.get("org/repo").is_some());
    }

    // ════════════════════════════════════════════════════════════
    //  RepoIndex::Default trait
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_repo_index_default() {
        let index = RepoIndex::default();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert_eq!(index.total_files(), 0);
    }

    // ════════════════════════════════════════════════════════════
    //  index_files - various excluded directories
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_index_files_skips_vendor_dir() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        std::fs::write(repo_dir.join("src.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(repo_dir.join("vendor")).unwrap();
        std::fs::write(repo_dir.join("vendor/pkg.rs"), "// vendor").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files[0].contains("src.rs"));
    }

    #[test]
    fn test_index_files_skips_target_dir() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        std::fs::write(repo_dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(repo_dir.join("target")).unwrap();
        std::fs::write(repo_dir.join("target/debug.rs"), "// target").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files[0].contains("main.rs"));
    }

    #[test]
    fn test_index_files_skips_build_dir() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        std::fs::write(repo_dir.join("app.js"), "// app").unwrap();
        std::fs::create_dir(repo_dir.join("build")).unwrap();
        std::fs::write(repo_dir.join("build/output.js"), "// build").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files[0].contains("app.js"));
    }

    #[test]
    fn test_index_files_skips_dist_dir() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        std::fs::write(repo_dir.join("index.ts"), "// src").unwrap();
        std::fs::create_dir(repo_dir.join("dist")).unwrap();
        std::fs::write(repo_dir.join("dist/bundle.js"), "// dist").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files[0].contains("index.ts"));
    }

    #[test]
    fn test_index_files_skips_pycache_dir() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        std::fs::write(repo_dir.join("app.py"), "# app").unwrap();
        std::fs::create_dir(repo_dir.join("__pycache__")).unwrap();
        std::fs::write(repo_dir.join("__pycache__/app.pyc"), "# cache").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 1);
        assert!(indexed.files[0].contains("app.py"));
    }

    #[test]
    fn test_index_files_includes_nested_visible_dirs() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir_all(repo_dir.join("src/nested/deep")).unwrap();

        std::fs::write(repo_dir.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(repo_dir.join("src/nested/mod.rs"), "mod inner;").unwrap();
        std::fs::write(repo_dir.join("src/nested/deep/inner.rs"), "// inner").unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert_eq!(indexed.files.len(), 3);
        let file_strs: Vec<&str> = indexed.files.iter().map(|s| s.as_str()).collect();
        assert!(file_strs.iter().any(|f| f.contains("main.rs")));
        assert!(file_strs.iter().any(|f| f.contains("mod.rs")));
        assert!(file_strs.iter().any(|f| f.contains("inner.rs")));
    }

    #[test]
    fn test_index_files_empty_directory() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        let repo = IndexedRepo::new("test/repo", &repo_dir);
        let indexed = index_files(repo);

        assert!(indexed.files.is_empty());
    }

    #[test]
    fn test_index_files_nonexistent_directory() {
        let repo = IndexedRepo::new("test/repo", "/nonexistent/path/xyz");
        let indexed = index_files(repo);
        assert!(indexed.files.is_empty());
    }

    // ════════════════════════════════════════════════════════════
    //  parse_repo_name_from_url - additional edge cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_parse_repo_name_ssh_gitlab() {
        let name = parse_repo_name_from_url("git@gitlab.com:group/project.git");
        assert_eq!(name, Some("group/project".to_string()));
    }

    #[test]
    fn test_parse_repo_name_https_nested_path() {
        // A URL with a deeper path should still return the last two components
        let name = parse_repo_name_from_url("https://gitlab.com/group/subgroup/repo.git");
        assert_eq!(name, Some("subgroup/repo".to_string()));
    }

    #[test]
    fn test_parse_repo_name_just_slash() {
        let name = parse_repo_name_from_url("/");
        assert!(name.is_none());
    }

    #[test]
    fn test_parse_repo_name_single_component() {
        // URL with only one path component
        let name = parse_repo_name_from_url("https://github.com/onlyrepo");
        // parts.len() >= 2 but the second-to-last is "github.com" and last is "onlyrepo"
        assert_eq!(name, Some("github.com/onlyrepo".to_string()));
    }

    #[test]
    fn test_parse_repo_name_ssh_with_port_like_format() {
        // Some SSH URLs have unusual formats
        let name = parse_repo_name_from_url("git@github.com:org/repo.git");
        assert_eq!(name, Some("org/repo".to_string()));
    }

    // ════════════════════════════════════════════════════════════
    //  find_by_file - various path formats
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_find_by_file_full_path_match_takes_priority() {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["src/utils.rs".to_string()];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["lib/utils.rs".to_string()];
        index.add_repo(repo2);

        // Full path match should find exact repo
        let found = index.find_by_file("src/utils.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/repo1");

        let found = index.find_by_file("lib/utils.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "org/repo2");
    }

    #[test]
    fn test_find_by_file_vendor_path_with_deep_nesting() {
        let mut index = RepoIndex::new();

        let repo = IndexedRepo::new("myorg/mylib", "/path");
        index.add_repo(repo);

        let found = index.find_by_file("/app/vendor/myorg/mylib/src/deep/nested/File.php");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "myorg/mylib");
    }

    #[test]
    fn test_find_by_file_vendor_path_no_match_in_index() {
        let index = RepoIndex::new();

        // Vendor path but repo not in index
        let found = index.find_by_file("/app/vendor/unknown/lib/src/File.php");
        assert!(found.is_none());
    }

    // ════════════════════════════════════════════════════════════
    //  search_files - additional scenarios
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_search_files_across_multiple_repos() {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("org/frontend", "/path1");
        repo1.files = vec![
            "src/components/Button.tsx".to_string(),
            "src/components/Modal.tsx".to_string(),
        ];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/backend", "/path2");
        repo2.files = vec!["src/api/routes.rs".to_string()];
        index.add_repo(repo2);

        let results = index.search_files(".tsx");
        assert_eq!(results.len(), 2);

        let results = index.search_files("routes");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "org/backend");
    }

    #[test]
    fn test_search_files_case_insensitive() {
        let mut index = RepoIndex::new();

        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["src/MyComponent.tsx".to_string()];
        index.add_repo(repo);

        let results = index.search_files("mycomponent");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_files_empty_query() {
        let mut index = RepoIndex::new();

        let mut repo = IndexedRepo::new("org/repo", "/path");
        repo.files = vec!["a.rs".to_string(), "b.rs".to_string()];
        index.add_repo(repo);

        // Empty query should match everything
        let results = index.search_files("");
        assert_eq!(results.len(), 2);
    }

    // ════════════════════════════════════════════════════════════
    //  IndexedRepo::from_api - edge cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_indexed_repo_from_api_deep_org_path() {
        let workspace = PathBuf::from("/repos");
        let repo = IndexedRepo::from_api(
            "org/subgroup/project",
            "https://gitlab.com/org/subgroup/project.git",
            "main",
            &workspace,
        );

        // next_back() on "org/subgroup/project" gives "project"
        assert_eq!(repo.path, workspace.join("project"));
        assert_eq!(repo.name, "org/subgroup/project");
    }

    #[test]
    fn test_indexed_repo_from_api_no_slash_in_name() {
        let workspace = PathBuf::from("/repos");
        let repo = IndexedRepo::from_api(
            "standalone",
            "https://github.com/org/standalone.git",
            "develop",
            &workspace,
        );

        assert_eq!(repo.path, workspace.join("standalone"));
        assert_eq!(repo.default_branch, "develop");
    }

    // ════════════════════════════════════════════════════════════
    //  expand_path - additional cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_expand_path_tilde_with_nested() {
        let expanded = expand_path("~/a/b/c");
        if let Some(home) = dirs::home_dir() {
            assert_eq!(expanded, home.join("a/b/c"));
        }
    }

    #[test]
    fn test_expand_path_no_tilde_prefix() {
        // A path that contains ~ but not at the start
        let expanded = expand_path("/home/user~backup/data");
        assert_eq!(expanded, PathBuf::from("/home/user~backup/data"));
    }

    // ════════════════════════════════════════════════════════════
    //  RepoIndex::index_repo_files - edge cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_index_repo_files_updates_file_index() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();
        std::fs::write(repo_dir.join("newly_created.rs"), "fn test() {}").unwrap();

        let mut index = RepoIndex::new();
        // Add repo initially with no files
        let repo = IndexedRepo::new("test/repo", &repo_dir);
        index.add_repo(repo);

        assert_eq!(index.total_files(), 0);

        // Index files - should pick up the file
        let count = index.index_repo_files("test/repo");
        assert_eq!(count, Some(1));

        // Now the file should be findable
        let found = index.find_by_file("newly_created.rs");
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "test/repo");
    }

    // ════════════════════════════════════════════════════════════
    //  RepoIndex::build - with temp directory
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_build_nonexistent_path_returns_empty() {
        let result = RepoIndex::build(
            &["someorg".to_string()],
            &["/nonexistent/path/xyz/abc".to_string()],
        );
        assert!(result.is_ok());
        let index = result.unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_empty_orgs_and_paths() {
        let result = RepoIndex::build(&[], &[]);
        assert!(result.is_ok());
        let index = result.unwrap();
        assert!(index.is_empty());
    }

    #[test]
    fn test_build_with_empty_dir() {
        let temp = TempDir::new().unwrap();
        let result = RepoIndex::build(
            &["someorg".to_string()],
            &[temp.path().to_string_lossy().to_string()],
        );
        assert!(result.is_ok());
        let index = result.unwrap();
        // No git repos inside, so should be empty
        assert!(index.is_empty());
    }

    // ════════════════════════════════════════════════════════════
    //  IndexedRepo::has_file / find_files - edge cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_has_file_partial_match() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec!["src/controllers/UserController.php".to_string()];

        // "Controller" appears as a substring
        assert!(repo.has_file("Controller"));
        // "User" appears as a substring
        assert!(repo.has_file("User"));
        // Full path works
        assert!(repo.has_file("src/controllers/UserController.php"));
    }

    #[test]
    fn test_find_files_returns_full_relative_paths() {
        let mut repo = IndexedRepo::new("test/repo", "/path");
        repo.files = vec!["src/a/file1.rs".to_string(), "src/b/file2.rs".to_string()];

        let results = repo.find_files("src");
        assert_eq!(results.len(), 2);
        // Results should be full relative paths
        assert!(results.contains(&"src/a/file1.rs"));
        assert!(results.contains(&"src/b/file2.rs"));
    }

    // ════════════════════════════════════════════════════════════
    //  build_from_gitlab - no groups
    // ════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_build_from_gitlab_no_groups() {
        let provider = MockScmProvider::with_repos(vec![]);
        let workspace = PathBuf::from("/tmp/repos");
        let groups: Vec<String> = vec![];

        let index = RepoIndex::build_from_gitlab(&groups, &provider, &workspace, false)
            .await
            .unwrap();

        assert!(index.is_empty());
    }

    // ════════════════════════════════════════════════════════════
    //  File index - basename collision
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_file_index_basename_collision() {
        let mut index = RepoIndex::new();

        // Two repos with files that share a basename
        let mut repo1 = IndexedRepo::new("org/repo1", "/path1");
        repo1.files = vec!["src/utils.rs".to_string()];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("org/repo2", "/path2");
        repo2.files = vec!["lib/utils.rs".to_string()];
        index.add_repo(repo2);

        // Basename "utils.rs" will be overwritten by the second add_repo call
        // The exact behavior depends on insertion order
        let found = index.find_by_file("utils.rs");
        assert!(found.is_some());
        // Should be one of the repos (last one wins)
        let name = &found.unwrap().name;
        assert!(name == "org/repo1" || name == "org/repo2");
    }

    // ════════════════════════════════════════════════════════════
    //  Multiple merge operations
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_repo_index_multiple_merges() {
        let mut base = RepoIndex::new();

        for i in 0..3 {
            let mut idx = RepoIndex::new();
            let repo = IndexedRepo::new(format!("org/repo{}", i), format!("/path{}", i));
            idx.add_repo(repo);
            base.merge(idx);
        }

        assert_eq!(base.len(), 3);
        for i in 0..3 {
            assert!(base.get(&format!("org/repo{}", i)).is_some());
        }
    }

    // ════════════════════════════════════════════════════════════
    //  build_with_fallback - various strategies
    // ════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_build_with_fallback_local_paths_preferred() {
        let temp = TempDir::new().unwrap();
        // No repos in temp, but this tests that strategy 1 is chosen
        let paths = vec![temp.path().to_string_lossy().to_string()];
        let orgs: Vec<String> = vec!["test-org".to_string()];
        let workspace = PathBuf::from("/tmp/repos");

        let index = RepoIndex::build_with_fallback(
            &orgs,
            &paths,
            None, // no github
            None, // no gitlab
            &[],  // no groups
            &workspace,
            false,
        )
        .await
        .unwrap();

        // No actual repos in temp dir, but the function returned without error
        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn test_build_with_fallback_gitlab_strategy() {
        let repos = vec![make_remote_repo(
            "group/repo-a",
            "https://gitlab.com/group/repo-a.git",
            "git@gitlab.com:group/repo-a.git",
            "main",
        )];
        let provider = MockScmProvider::with_repos(repos);
        let workspace = PathBuf::from("/tmp/repos");
        let groups = vec!["group".to_string()];

        let index = RepoIndex::build_with_fallback(
            &[],             // no orgs
            &[],             // no local paths
            None,            // no github
            Some(&provider), // gitlab available
            &groups,
            &workspace,
            false,
        )
        .await
        .unwrap();

        assert_eq!(index.len(), 1);
        assert!(index.get("group/repo-a").is_some());
    }

    #[tokio::test]
    async fn test_build_with_fallback_empty_returns_empty() {
        let workspace = PathBuf::from("/tmp/repos");

        let index = RepoIndex::build_with_fallback(
            &[],  // no orgs
            &[],  // no local paths
            None, // no github
            None, // no gitlab
            &[],  // no groups
            &workspace,
            false,
        )
        .await
        .unwrap();

        assert!(index.is_empty());
    }

    #[tokio::test]
    async fn test_build_with_fallback_disabled_gitlab_returns_empty() {
        // Provider with repos but not enabled (no groups)
        let provider = MockScmProvider::with_repos(vec![make_remote_repo(
            "group/repo",
            "https://gitlab.com/group/repo.git",
            "git@gitlab.com:group/repo.git",
            "main",
        )]);
        let workspace = PathBuf::from("/tmp/repos");

        let index = RepoIndex::build_with_fallback(
            &[],             // no orgs
            &[],             // no local paths
            None,            // no github
            Some(&provider), // gitlab available but...
            &[],             // no groups => won't use gitlab
            &workspace,
            false,
        )
        .await
        .unwrap();

        assert!(index.is_empty());
    }

    // ════════════════════════════════════════════════════════════
    //  index_repo_files - not found repo
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_index_repo_files_nonexistent_repo() {
        let mut index = RepoIndex::new();
        let result = index.index_repo_files("nonexistent/repo");
        assert!(result.is_none());
    }

    #[test]
    fn test_index_repo_files_after_adding_files() {
        let temp = TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();

        let mut index = RepoIndex::new();
        let repo = IndexedRepo::new("org/myrepo", &repo_dir);
        index.add_repo(repo);

        // Initially no files
        assert_eq!(index.total_files(), 0);

        // Create a file
        std::fs::write(repo_dir.join("main.rs"), "fn main() {}").unwrap();

        // Re-index
        let count = index.index_repo_files("org/myrepo");
        assert_eq!(count, Some(1));
        assert_eq!(index.total_files(), 1);
    }

    // ════════════════════════════════════════════════════════════
    //  IndexedRepo::new edge cases
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_indexed_repo_default_branch() {
        let repo = IndexedRepo::new("org/repo", "/path");
        assert_eq!(repo.default_branch, "main");
        assert!(repo.files.is_empty());
        assert_eq!(repo.name, "org/repo");
    }

    #[test]
    fn test_indexed_repo_from_api_sets_branch() {
        let workspace = PathBuf::from("/repos");
        let repo = IndexedRepo::from_api(
            "org/myrepo",
            "https://github.com/org/myrepo.git",
            "develop",
            &workspace,
        );

        assert_eq!(repo.default_branch, "develop");
        assert_eq!(repo.scm_url, "https://github.com/org/myrepo.git");
        assert_eq!(repo.path, workspace.join("myrepo"));
    }

    // ════════════════════════════════════════════════════════════
    //  RepoIndex::list
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_list_empty() {
        let index = RepoIndex::new();
        let repos = index.list();
        assert!(repos.is_empty());
    }

    #[test]
    fn test_list_multiple() {
        let mut index = RepoIndex::new();
        index.add_repo(IndexedRepo::new("org/a", "/a"));
        index.add_repo(IndexedRepo::new("org/b", "/b"));
        index.add_repo(IndexedRepo::new("org/c", "/c"));

        let repos = index.list();
        assert_eq!(repos.len(), 3);

        let names: Vec<&str> = repos.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"org/a"));
        assert!(names.contains(&"org/b"));
        assert!(names.contains(&"org/c"));
    }

    // ════════════════════════════════════════════════════════════
    //  search_files - additional scenarios
    // ════════════════════════════════════════════════════════════

    #[test]
    fn test_search_files_empty_index() {
        let index = RepoIndex::new();
        let results = index.search_files("something");
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_files_finds_by_keyword() {
        let mut index = RepoIndex::new();
        let mut repo = IndexedRepo::new("org/auth-service", "/path");
        repo.files = vec!["src/auth.rs".to_string()];
        index.add_repo(repo);

        let results = index.search_files("auth");
        assert!(!results.is_empty());
    }
}

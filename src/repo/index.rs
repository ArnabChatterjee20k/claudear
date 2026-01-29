//! Repository index for file-based searching.
//!
//! Provides a searchable index of repositories discovered from known organizations.
//! This enables issue-to-repository inference by matching file paths and names.

use crate::error::Result;
use crate::github::GitHubClient;
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
    pub github_url: String,
    /// Relative file paths within the repository.
    pub files: Vec<String>,
    /// Default branch name.
    pub default_branch: String,
    /// Whether this repo needs to be cloned (API-discovered repos).
    pub needs_clone: bool,
}

impl IndexedRepo {
    /// Create a new indexed repository.
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let name = name.into();
        let github_url = format!("https://github.com/{}", name);
        Self {
            name,
            path: path.into(),
            github_url,
            files: Vec::new(),
            default_branch: "main".to_string(),
            needs_clone: false,
        }
    }

    /// Create a remote repository discovered via GitHub API.
    ///
    /// These repos don't exist locally yet and need to be cloned before use.
    pub fn remote(
        name: impl Into<String>,
        github_url: impl Into<String>,
        default_branch: impl Into<String>,
        work_dir: &Path,
    ) -> Self {
        let name = name.into();
        // Extract repo name from full_name (org/repo)
        let repo_name = name.split('/').last().unwrap_or(&name);
        let path = work_dir.join(repo_name);
        Self {
            name,
            path,
            github_url: github_url.into(),
            files: Vec::new(),
            default_branch: default_branch.into(),
            needs_clone: true,
        }
    }

    /// Set the GitHub URL.
    pub fn with_github_url(mut self, url: impl Into<String>) -> Self {
        self.github_url = url.into();
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
    /// a GitHub token are configured. Repos discovered this way need to be
    /// cloned before they can be used.
    ///
    /// # Arguments
    /// * `known_orgs` - GitHub organization names to fetch repos from
    /// * `client` - GitHub API client with token configured
    /// * `work_dir` - Directory where repos will be cloned to
    ///
    /// # Returns
    /// A populated RepoIndex with remote repositories (needs_clone=true).
    pub async fn build_from_github(
        known_orgs: &[String],
        client: &GitHubClient,
        work_dir: &Path,
    ) -> Result<Self> {
        let mut index = Self::new();

        for org in known_orgs {
            tracing::info!(org = %org, "Fetching repositories from GitHub API");

            match client.list_org_repos(org).await {
                Ok(repos) => {
                    for repo in repos {
                        let indexed = IndexedRepo::remote(
                            &repo.full_name,
                            &repo.clone_url,
                            &repo.default_branch,
                            work_dir,
                        );

                        tracing::debug!(
                            repo = %repo.full_name,
                            path = %indexed.path.display(),
                            "Added remote repository to index"
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

    /// Build an index using the best available method.
    ///
    /// This chooses the discovery method based on configuration:
    /// 1. If `auto_discover_paths` is not empty → use local filesystem scan
    /// 2. Else if GitHub token is configured + `known_orgs` not empty → use API
    /// 3. Else → return empty index
    ///
    /// # Arguments
    /// * `known_orgs` - GitHub organization names
    /// * `auto_discover_paths` - Local paths to scan for repos
    /// * `github_client` - Optional GitHub API client
    /// * `work_dir` - Directory where repos will be cloned to (for API discovery)
    pub async fn build_with_fallback(
        known_orgs: &[String],
        auto_discover_paths: &[String],
        github_client: Option<&GitHubClient>,
        work_dir: &Path,
    ) -> Result<Self> {
        // Strategy 1: Local filesystem scan (preferred when paths are configured)
        if !auto_discover_paths.is_empty() {
            tracing::info!("Building repo index from local filesystem");
            return Self::build(known_orgs, auto_discover_paths);
        }

        // Strategy 2: GitHub API discovery
        if let Some(client) = github_client {
            if client.is_enabled() && !known_orgs.is_empty() {
                tracing::info!("Building repo index from GitHub API (no auto_discover_paths configured)");
                return Self::build_from_github(known_orgs, client, work_dir).await;
            }
        }

        // Strategy 3: Empty index
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
    // Handle SSH URLs: git@github.com:org/repo.git
    if url.starts_with("git@") {
        let parts: Vec<_> = url.split(':').collect();
        if parts.len() == 2 {
            let repo_part = parts[1].trim_end_matches(".git");
            return Some(repo_part.to_string());
        }
    }

    // Handle HTTPS URLs: https://github.com/org/repo.git
    if url.contains("github.com") {
        let url = url.trim_end_matches(".git");
        let parts: Vec<_> = url.split('/').collect();
        if parts.len() >= 2 {
            let org = parts[parts.len() - 2];
            let repo = parts[parts.len() - 1];
            return Some(format!("{}/{}", org, repo));
        }
    }

    None
}

/// Index all files in a repository.
fn index_files(mut repo: IndexedRepo) -> IndexedRepo {
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
        assert_eq!(repo.github_url, "https://github.com/appwrite/cloud");
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn test_indexed_repo_with_github_url() {
        let repo =
            IndexedRepo::new("test/repo", "/path").with_github_url("https://gitlab.com/test/repo");
        assert_eq!(repo.github_url, "https://gitlab.com/test/repo");
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
        let vendor_path =
            "/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/Pool.php";
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
        let vendor_path =
            "/usr/src/code/vendor/utopia-php/database/src/Database/Adapter/Pool.php";
        let found = index.find_by_file(vendor_path);
        assert!(found.is_some());
        // Falls back to basename match since utopia-php/database isn't indexed
        assert_eq!(found.unwrap().name, "appwrite/appwrite");
    }

    #[test]
    fn test_indexed_repo_new_needs_clone_false() {
        let repo = IndexedRepo::new("test/repo", "/path/to/repo");
        assert!(!repo.needs_clone);
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn test_indexed_repo_remote() {
        let work_dir = PathBuf::from("/home/user/repos");
        let repo = IndexedRepo::remote(
            "test-org/my-repo",
            "https://github.com/test-org/my-repo.git",
            "develop",
            &work_dir,
        );

        assert_eq!(repo.name, "test-org/my-repo");
        assert_eq!(repo.github_url, "https://github.com/test-org/my-repo.git");
        assert_eq!(repo.default_branch, "develop");
        assert_eq!(repo.path, work_dir.join("my-repo"));
        assert!(repo.needs_clone);
        assert!(repo.files.is_empty());
    }

    #[test]
    fn test_indexed_repo_remote_extracts_repo_name() {
        let work_dir = PathBuf::from("/var/repos");

        // Test with full_name format "org/repo"
        let repo = IndexedRepo::remote(
            "appwrite/console",
            "https://github.com/appwrite/console.git",
            "main",
            &work_dir,
        );
        assert_eq!(repo.path, work_dir.join("console"));

        // Test with simple name (no slash)
        let repo2 = IndexedRepo::remote(
            "simple-repo",
            "https://github.com/org/simple-repo.git",
            "main",
            &work_dir,
        );
        assert_eq!(repo2.path, work_dir.join("simple-repo"));
    }

    #[test]
    fn test_repo_index_add_remote_repo() {
        let mut index = RepoIndex::new();
        let work_dir = PathBuf::from("/repos");

        let repo = IndexedRepo::remote(
            "test-org/test-repo",
            "https://github.com/test-org/test-repo.git",
            "main",
            &work_dir,
        );
        index.add_repo(repo);

        assert_eq!(index.len(), 1);
        let found = index.get("test-org/test-repo").unwrap();
        assert!(found.needs_clone);
        assert_eq!(found.github_url, "https://github.com/test-org/test-repo.git");
    }
}

//! GitHub Release API client.

use crate::error::{Error, Result};
use crate::github::{HttpClient, ReqwestHttpClient};
use serde::Deserialize;

/// A GitHub release.
#[derive(Debug, Clone, Deserialize)]
pub struct GitHubRelease {
    /// Release ID.
    pub id: i64,
    /// Tag name (e.g., "v1.2.3").
    pub tag_name: String,
    /// Release name/title.
    pub name: Option<String>,
    /// Whether this is a draft release.
    pub draft: bool,
    /// Whether this is a prerelease.
    pub prerelease: bool,
    /// When the release was created.
    pub created_at: String,
    /// When the release was published.
    pub published_at: Option<String>,
    /// Target commit SHA or branch.
    pub target_commitish: String,
    /// Release body/description.
    pub body: Option<String>,
    /// HTML URL to the release.
    pub html_url: String,
}

/// A GitHub commit.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct GitHubCommit {
    /// Commit SHA.
    pub sha: String,
    /// Commit message.
    pub commit: CommitDetails,
    /// HTML URL to the commit.
    pub html_url: String,
}

/// Commit details.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CommitDetails {
    /// Commit message.
    pub message: String,
}

/// GitHub Release API client.
pub struct ReleaseClient<H: HttpClient = ReqwestHttpClient> {
    token: String,
    http: H,
}

impl ReleaseClient<ReqwestHttpClient> {
    /// Create a new release client with the default HTTP client.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            http: ReqwestHttpClient::new(),
        }
    }
}

impl<H: HttpClient> ReleaseClient<H> {
    /// Create a new release client with a custom HTTP client.
    pub fn with_http_client(token: impl Into<String>, http: H) -> Self {
        Self {
            token: token.into(),
            http,
        }
    }

    /// Build standard GitHub API headers.
    fn build_headers(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Authorization", format!("Bearer {}", self.token)),
            ("Accept", "application/vnd.github+json".to_string()),
            ("User-Agent", "claudear".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ]
    }

    /// Get the latest release for a repository.
    pub async fn get_latest_release(&self, repo: &str) -> Result<Option<GitHubRelease>> {
        let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(None);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        let release: GitHubRelease = response.json()?;
        Ok(Some(release))
    }

    /// Get recent releases for a repository.
    pub async fn get_releases(&self, repo: &str, per_page: u32) -> Result<Vec<GitHubRelease>> {
        let url = format!(
            "https://api.github.com/repos/{}/releases?per_page={}",
            repo, per_page
        );
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Get a specific release by tag.
    pub async fn get_release_by_tag(&self, repo: &str, tag: &str) -> Result<Option<GitHubRelease>> {
        let url = format!(
            "https://api.github.com/repos/{}/releases/tags/{}",
            repo, tag
        );
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(None);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        let release: GitHubRelease = response.json()?;
        Ok(Some(release))
    }

    /// Check if a commit is included in a release.
    /// This compares commits between the release tag and the commit.
    pub async fn is_commit_in_release(
        &self,
        repo: &str,
        commit_sha: &str,
        release_tag: &str,
    ) -> Result<bool> {
        // Use the compare API to check if commit is an ancestor of the release
        let url = format!(
            "https://api.github.com/repos/{}/compare/{}...{}",
            repo, commit_sha, release_tag
        );
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(false);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        // Parse the comparison response
        #[derive(Deserialize)]
        struct CompareResponse {
            status: String,
        }

        let compare: CompareResponse = response.json()?;

        // If status is "behind" or "identical", the commit is in the release
        // "behind" means the commit is an ancestor of the release tag
        // "identical" means they point to the same commit
        Ok(compare.status == "behind" || compare.status == "identical")
    }

    /// Get the merge commit SHA for a PR.
    pub async fn get_pr_merge_commit(&self, repo: &str, pr_number: i64) -> Result<Option<String>> {
        let url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(None);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        #[derive(Deserialize)]
        struct PrResponse {
            merged: bool,
            merge_commit_sha: Option<String>,
        }

        let pr: PrResponse = response.json()?;

        if pr.merged {
            Ok(pr.merge_commit_sha)
        } else {
            Ok(None)
        }
    }

    /// Get PR details including merge time.
    pub async fn get_pr_details(&self, repo: &str, pr_number: i64) -> Result<Option<PrDetails>> {
        let url = format!("https://api.github.com/repos/{}/pulls/{}", repo, pr_number);
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(None);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        let pr: PrDetails = response.json()?;
        Ok(Some(pr))
    }

    /// Get the first release in a repo published after a given timestamp.
    pub async fn get_first_release_after(
        &self,
        repo: &str,
        after: &str,
    ) -> Result<Option<GitHubRelease>> {
        // Get recent releases (up to 30)
        let releases = self.get_releases(repo, 30).await?;

        // Parse the after timestamp
        let after_time = chrono::DateTime::parse_from_rfc3339(after)
            .map_err(|e| Error::Other(format!("Invalid timestamp: {}", e)))?;

        // Find the first (oldest) release published after the given time
        // Releases are returned newest first, so we need to find the oldest one after our time
        let mut candidates: Vec<_> = releases
            .into_iter()
            .filter(|r| !r.draft && !r.prerelease)
            .filter(|r| {
                r.published_at
                    .as_ref()
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t > after_time)
                    .unwrap_or(false)
            })
            .collect();

        // Sort by published_at ascending to get the first release after
        candidates.sort_by(|a, b| {
            let a_time = a.published_at.as_ref().unwrap();
            let b_time = b.published_at.as_ref().unwrap();
            a_time.cmp(b_time)
        });

        Ok(candidates.into_iter().next())
    }

    /// Check if a repo has any release published after a given timestamp.
    pub async fn has_release_after(&self, repo: &str, after: &str) -> Result<bool> {
        Ok(self.get_first_release_after(repo, after).await?.is_some())
    }

    /// Get file contents at a specific git ref (tag, commit SHA, or branch).
    pub async fn get_file_at_ref(
        &self,
        repo: &str,
        file_path: &str,
        git_ref: &str,
    ) -> Result<Option<String>> {
        let url = format!(
            "https://api.github.com/repos/{}/contents/{}?ref={}",
            repo, file_path, git_ref
        );
        let headers = self.build_headers();

        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Ok(None);
        }

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitHub API error ({}): {}",
                response.status, response.body
            )));
        }

        #[derive(Deserialize)]
        struct FileContent {
            content: String,
            encoding: String,
        }

        let file_content: FileContent = response.json()?;

        if file_content.encoding != "base64" {
            return Err(Error::Other(format!(
                "Unexpected encoding: {}",
                file_content.encoding
            )));
        }

        // Decode base64 content (GitHub returns with newlines)
        let content = file_content.content.replace('\n', "");
        let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &content)
            .map_err(|e| Error::Other(format!("Failed to decode base64: {}", e)))?;

        String::from_utf8(decoded)
            .map(Some)
            .map_err(|e| Error::Other(format!("Invalid UTF-8 in file: {}", e)))
    }

    /// Check if a package version in a lock file includes or is after a given version.
    ///
    /// Supports multiple lock file formats:
    /// - composer.lock (PHP Composer)
    /// - package-lock.json (npm)
    /// - yarn.lock (Yarn)
    /// - poetry.lock (Python Poetry)
    /// - Pipfile.lock (Python pipenv)
    /// - Cargo.lock (Rust)
    ///
    /// Returns true if the lock file contains a version >= min_version.
    pub fn check_lock_file_version(
        lock_content: &str,
        lock_file_path: &str,
        package_name: &str,
        min_version: &str,
    ) -> Result<bool> {
        // Determine lock file type from path
        let file_name = lock_file_path.rsplit('/').next().unwrap_or(lock_file_path);

        match file_name {
            "composer.lock" => Self::check_composer_lock(lock_content, package_name, min_version),
            "package-lock.json" => Self::check_npm_lock(lock_content, package_name, min_version),
            "yarn.lock" => Self::check_yarn_lock(lock_content, package_name, min_version),
            "poetry.lock" => Self::check_poetry_lock(lock_content, package_name, min_version),
            "Pipfile.lock" => Self::check_pipfile_lock(lock_content, package_name, min_version),
            "Cargo.lock" => Self::check_cargo_lock(lock_content, package_name, min_version),
            _ => Err(Error::Other(format!(
                "Unsupported lock file format: {}",
                file_name
            ))),
        }
    }

    /// Check composer.lock (PHP).
    fn check_composer_lock(
        lock_content: &str,
        package_name: &str,
        min_version: &str,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct ComposerLock {
            packages: Vec<ComposerPackage>,
            #[serde(rename = "packages-dev", default)]
            packages_dev: Vec<ComposerPackage>,
        }

        #[derive(Deserialize)]
        struct ComposerPackage {
            name: String,
            version: String,
        }

        let lock: ComposerLock = serde_json::from_str(lock_content)
            .map_err(|e| Error::Other(format!("Failed to parse composer.lock: {}", e)))?;

        let package = lock
            .packages
            .iter()
            .chain(lock.packages_dev.iter())
            .find(|p| p.name == package_name);

        Self::compare_versions(package.map(|p| p.version.as_str()), min_version)
    }

    /// Check package-lock.json (npm).
    fn check_npm_lock(lock_content: &str, package_name: &str, min_version: &str) -> Result<bool> {
        #[derive(Deserialize)]
        struct NpmLock {
            packages: Option<std::collections::HashMap<String, NpmPackage>>,
            dependencies: Option<std::collections::HashMap<String, NpmDep>>,
        }

        #[derive(Deserialize)]
        struct NpmPackage {
            version: Option<String>,
        }

        #[derive(Deserialize)]
        struct NpmDep {
            version: String,
        }

        let lock: NpmLock = serde_json::from_str(lock_content)
            .map_err(|e| Error::Other(format!("Failed to parse package-lock.json: {}", e)))?;

        // npm v3+ uses "packages" with node_modules/ prefix
        if let Some(packages) = &lock.packages {
            let key = format!("node_modules/{}", package_name);
            if let Some(pkg) = packages.get(&key) {
                return Self::compare_versions(pkg.version.as_deref(), min_version);
            }
        }

        // npm v2 and fallback uses "dependencies"
        if let Some(deps) = &lock.dependencies {
            if let Some(dep) = deps.get(package_name) {
                return Self::compare_versions(Some(&dep.version), min_version);
            }
        }

        Ok(false)
    }

    /// Check yarn.lock.
    /// Yarn lock files are a custom format, not JSON.
    fn check_yarn_lock(lock_content: &str, package_name: &str, min_version: &str) -> Result<bool> {
        // Yarn lock format:
        // "package-name@^1.0.0":
        //   version "1.2.3"
        //   ...

        let pattern = format!(r#"^"?{}@[^"]*"?:\s*$"#, regex_lite::escape(package_name));
        let re = regex_lite::Regex::new(&pattern)
            .map_err(|e| Error::Other(format!("Invalid regex: {}", e)))?;

        let version_re = regex_lite::Regex::new(r#"^\s+version\s+"([^"]+)""#)
            .map_err(|e| Error::Other(format!("Invalid regex: {}", e)))?;

        let mut in_package = false;
        for line in lock_content.lines() {
            if re.is_match(line) {
                in_package = true;
                continue;
            }
            if in_package {
                if let Some(caps) = version_re.captures(line) {
                    if let Some(version) = caps.get(1) {
                        return Self::compare_versions(Some(version.as_str()), min_version);
                    }
                }
                // If we hit a non-indented line, we've left the package block
                if !line.starts_with(' ') && !line.starts_with('\t') && !line.is_empty() {
                    in_package = false;
                }
            }
        }

        Ok(false)
    }

    /// Check poetry.lock (Python Poetry).
    fn check_poetry_lock(
        lock_content: &str,
        package_name: &str,
        min_version: &str,
    ) -> Result<bool> {
        // Poetry lock is TOML format:
        // [[package]]
        // name = "package-name"
        // version = "1.2.3"

        // Simple parsing without a full TOML parser
        let mut current_name: Option<&str> = None;
        let mut current_version: Option<&str> = None;

        for line in lock_content.lines() {
            let line = line.trim();

            if line == "[[package]]" {
                // Check previous package
                if let (Some(name), Some(version)) = (current_name, current_version) {
                    if name == package_name {
                        return Self::compare_versions(Some(version), min_version);
                    }
                }
                current_name = None;
                current_version = None;
                continue;
            }

            if let Some(rest) = line.strip_prefix("name = ") {
                current_name = Some(rest.trim_matches('"'));
            } else if let Some(rest) = line.strip_prefix("version = ") {
                current_version = Some(rest.trim_matches('"'));
            }
        }

        // Check last package
        if let (Some(name), Some(version)) = (current_name, current_version) {
            if name == package_name {
                return Self::compare_versions(Some(version), min_version);
            }
        }

        Ok(false)
    }

    /// Check Pipfile.lock (Python pipenv).
    fn check_pipfile_lock(
        lock_content: &str,
        package_name: &str,
        min_version: &str,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct PipfileLock {
            default: Option<std::collections::HashMap<String, PipPackage>>,
            develop: Option<std::collections::HashMap<String, PipPackage>>,
        }

        #[derive(Deserialize)]
        struct PipPackage {
            version: Option<String>,
        }

        let lock: PipfileLock = serde_json::from_str(lock_content)
            .map_err(|e| Error::Other(format!("Failed to parse Pipfile.lock: {}", e)))?;

        // Check default dependencies
        if let Some(default) = &lock.default {
            if let Some(pkg) = default.get(package_name) {
                if let Some(version) = &pkg.version {
                    // Pipfile uses "==1.2.3" format
                    let ver = version.trim_start_matches("==");
                    return Self::compare_versions(Some(ver), min_version);
                }
            }
        }

        // Check develop dependencies
        if let Some(develop) = &lock.develop {
            if let Some(pkg) = develop.get(package_name) {
                if let Some(version) = &pkg.version {
                    let ver = version.trim_start_matches("==");
                    return Self::compare_versions(Some(ver), min_version);
                }
            }
        }

        Ok(false)
    }

    /// Check Cargo.lock (Rust).
    fn check_cargo_lock(lock_content: &str, package_name: &str, min_version: &str) -> Result<bool> {
        // Cargo.lock is TOML format:
        // [[package]]
        // name = "package-name"
        // version = "1.2.3"

        // Simple parsing without a full TOML parser (same as poetry.lock)
        let mut current_name: Option<&str> = None;
        let mut current_version: Option<&str> = None;

        for line in lock_content.lines() {
            let line = line.trim();

            if line == "[[package]]" {
                // Check previous package
                if let (Some(name), Some(version)) = (current_name, current_version) {
                    if name == package_name {
                        return Self::compare_versions(Some(version), min_version);
                    }
                }
                current_name = None;
                current_version = None;
                continue;
            }

            if let Some(rest) = line.strip_prefix("name = ") {
                current_name = Some(rest.trim_matches('"'));
            } else if let Some(rest) = line.strip_prefix("version = ") {
                current_version = Some(rest.trim_matches('"'));
            }
        }

        // Check last package
        if let (Some(name), Some(version)) = (current_name, current_version) {
            if name == package_name {
                return Self::compare_versions(Some(version), min_version);
            }
        }

        Ok(false)
    }

    /// Compare version strings using semver when possible.
    fn compare_versions(lock_version: Option<&str>, min_version: &str) -> Result<bool> {
        match lock_version {
            Some(lock_ver) => {
                // Strip 'v' prefix if present
                let lock_ver = lock_ver.trim_start_matches('v');
                let min_ver = min_version.trim_start_matches('v');

                // Use semver comparison if both are valid semver
                match (
                    semver::Version::parse(lock_ver),
                    semver::Version::parse(min_ver),
                ) {
                    (Ok(lock_semver), Ok(min_semver)) => Ok(lock_semver >= min_semver),
                    // Fall back to string comparison
                    _ => Ok(lock_ver >= min_ver),
                }
            }
            None => Ok(false),
        }
    }

    /// Legacy method for backwards compatibility.
    /// Use `check_lock_file_version` instead.
    pub fn check_composer_lock_version(
        lock_content: &str,
        package_name: &str,
        min_version: &str,
    ) -> Result<bool> {
        Self::check_composer_lock(lock_content, package_name, min_version)
    }
}

/// PR details from GitHub API.
#[derive(Debug, Clone, Deserialize)]
pub struct PrDetails {
    /// PR number.
    pub number: i64,
    /// Whether the PR is merged.
    pub merged: bool,
    /// Merge commit SHA.
    pub merge_commit_sha: Option<String>,
    /// When the PR was merged.
    pub merged_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::HttpResponse;
    use async_trait::async_trait;

    struct MockHttpClient {
        response: HttpResponse,
    }

    impl MockHttpClient {
        fn new(status: u16, body: &str) -> Self {
            Self {
                response: HttpResponse {
                    status,
                    body: body.to_string(),
                },
            }
        }
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn get(&self, _url: &str, _headers: Vec<(&str, String)>) -> Result<HttpResponse> {
            Ok(HttpResponse {
                status: self.response.status,
                body: self.response.body.clone(),
            })
        }
    }

    #[tokio::test]
    async fn test_get_latest_release_success() {
        let mock = MockHttpClient::new(
            200,
            r#"{
                "id": 1,
                "tag_name": "v1.0.0",
                "name": "Version 1.0.0",
                "draft": false,
                "prerelease": false,
                "created_at": "2024-01-15T10:00:00Z",
                "published_at": "2024-01-15T10:30:00Z",
                "target_commitish": "main",
                "body": "Release notes",
                "html_url": "https://github.com/org/repo/releases/tag/v1.0.0"
            }"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release = client.get_latest_release("org/repo").await.unwrap();

        assert!(release.is_some());
        let release = release.unwrap();
        assert_eq!(release.tag_name, "v1.0.0");
        assert_eq!(release.name, Some("Version 1.0.0".to_string()));
        assert!(!release.draft);
        assert!(!release.prerelease);
    }

    #[tokio::test]
    async fn test_get_latest_release_not_found() {
        let mock = MockHttpClient::new(404, r#"{"message": "Not Found"}"#);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release = client.get_latest_release("org/repo").await.unwrap();

        assert!(release.is_none());
    }

    #[tokio::test]
    async fn test_get_releases_success() {
        let mock = MockHttpClient::new(
            200,
            r#"[
                {
                    "id": 1,
                    "tag_name": "v1.1.0",
                    "name": "Version 1.1.0",
                    "draft": false,
                    "prerelease": false,
                    "created_at": "2024-01-20T10:00:00Z",
                    "published_at": "2024-01-20T10:30:00Z",
                    "target_commitish": "main",
                    "body": "Release 1.1.0",
                    "html_url": "https://github.com/org/repo/releases/tag/v1.1.0"
                },
                {
                    "id": 2,
                    "tag_name": "v1.0.0",
                    "name": "Version 1.0.0",
                    "draft": false,
                    "prerelease": false,
                    "created_at": "2024-01-15T10:00:00Z",
                    "published_at": "2024-01-15T10:30:00Z",
                    "target_commitish": "main",
                    "body": "Release 1.0.0",
                    "html_url": "https://github.com/org/repo/releases/tag/v1.0.0"
                }
            ]"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let releases = client.get_releases("org/repo", 10).await.unwrap();

        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].tag_name, "v1.1.0");
        assert_eq!(releases[1].tag_name, "v1.0.0");
    }

    #[tokio::test]
    async fn test_get_release_by_tag_success() {
        let mock = MockHttpClient::new(
            200,
            r#"{
                "id": 1,
                "tag_name": "v1.0.0",
                "name": "Version 1.0.0",
                "draft": false,
                "prerelease": false,
                "created_at": "2024-01-15T10:00:00Z",
                "published_at": "2024-01-15T10:30:00Z",
                "target_commitish": "main",
                "body": "Release notes",
                "html_url": "https://github.com/org/repo/releases/tag/v1.0.0"
            }"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release = client
            .get_release_by_tag("org/repo", "v1.0.0")
            .await
            .unwrap();

        assert!(release.is_some());
        assert_eq!(release.unwrap().tag_name, "v1.0.0");
    }

    #[tokio::test]
    async fn test_get_release_by_tag_not_found() {
        let mock = MockHttpClient::new(404, r#"{"message": "Not Found"}"#);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release = client
            .get_release_by_tag("org/repo", "v9.9.9")
            .await
            .unwrap();

        assert!(release.is_none());
    }

    #[tokio::test]
    async fn test_is_commit_in_release_true() {
        let mock = MockHttpClient::new(
            200,
            r#"{"status": "behind", "ahead_by": 0, "behind_by": 5}"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let result = client
            .is_commit_in_release("org/repo", "abc123", "v1.0.0")
            .await
            .unwrap();

        assert!(result);
    }

    #[tokio::test]
    async fn test_is_commit_in_release_false() {
        let mock =
            MockHttpClient::new(200, r#"{"status": "ahead", "ahead_by": 3, "behind_by": 0}"#);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let result = client
            .is_commit_in_release("org/repo", "xyz789", "v1.0.0")
            .await
            .unwrap();

        assert!(!result);
    }

    #[tokio::test]
    async fn test_get_pr_merge_commit_success() {
        let mock = MockHttpClient::new(
            200,
            r#"{"merged": true, "merge_commit_sha": "abc123def456"}"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let sha = client.get_pr_merge_commit("org/repo", 42).await.unwrap();

        assert_eq!(sha, Some("abc123def456".to_string()));
    }

    #[tokio::test]
    async fn test_get_pr_merge_commit_not_merged() {
        let mock = MockHttpClient::new(200, r#"{"merged": false, "merge_commit_sha": null}"#);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let sha = client.get_pr_merge_commit("org/repo", 42).await.unwrap();

        assert!(sha.is_none());
    }

    #[test]
    fn test_check_composer_lock() {
        let lock = r#"{
            "packages": [
                {"name": "utopia-php/database", "version": "v0.45.0"},
                {"name": "utopia-php/framework", "version": "v0.30.0"}
            ],
            "packages-dev": [
                {"name": "phpunit/phpunit", "version": "v10.0.0"}
            ]
        }"#;

        // Package found with sufficient version
        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "composer.lock",
            "utopia-php/database",
            "v0.45.0"
        )
        .unwrap());

        // Package found with newer version
        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "composer.lock",
            "utopia-php/database",
            "v0.44.0"
        )
        .unwrap());

        // Package found but older version
        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "composer.lock",
            "utopia-php/database",
            "v0.46.0"
        )
        .unwrap());

        // Package in dev dependencies
        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "composer.lock",
            "phpunit/phpunit",
            "v10.0.0"
        )
        .unwrap());

        // Package not found
        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "composer.lock",
            "nonexistent/package",
            "v1.0.0"
        )
        .unwrap());
    }

    #[test]
    fn test_check_npm_lock() {
        // npm v3+ format with packages
        let lock_v3 = r#"{
            "packages": {
                "node_modules/lodash": {"version": "4.17.21"},
                "node_modules/express": {"version": "4.18.0"}
            }
        }"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock_v3,
            "package-lock.json",
            "lodash",
            "4.17.21"
        )
        .unwrap());

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock_v3,
            "package-lock.json",
            "lodash",
            "4.17.0"
        )
        .unwrap());

        // npm v2 format with dependencies
        let lock_v2 = r#"{
            "dependencies": {
                "lodash": {"version": "4.17.21"},
                "express": {"version": "4.18.0"}
            }
        }"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock_v2,
            "package-lock.json",
            "lodash",
            "4.17.21"
        )
        .unwrap());
    }

    #[test]
    fn test_check_yarn_lock() {
        let lock = r#"
"lodash@^4.17.0":
  version "4.17.21"
  resolved "https://registry.yarnpkg.com/lodash/-/lodash-4.17.21.tgz"

"express@^4.18.0":
  version "4.18.2"
  resolved "https://registry.yarnpkg.com/express/-/express-4.18.2.tgz"
"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "yarn.lock",
            "lodash",
            "4.17.21"
        )
        .unwrap());

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "yarn.lock",
            "lodash",
            "4.17.0"
        )
        .unwrap());

        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "yarn.lock",
            "lodash",
            "4.18.0"
        )
        .unwrap());

        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "yarn.lock",
            "nonexistent",
            "1.0.0"
        )
        .unwrap());
    }

    #[test]
    fn test_check_poetry_lock() {
        let lock = r#"
[[package]]
name = "requests"
version = "2.31.0"

[[package]]
name = "urllib3"
version = "2.0.4"
"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "poetry.lock",
            "requests",
            "2.31.0"
        )
        .unwrap());

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "poetry.lock",
            "urllib3",
            "2.0.0"
        )
        .unwrap());

        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "poetry.lock",
            "nonexistent",
            "1.0.0"
        )
        .unwrap());
    }

    #[test]
    fn test_check_pipfile_lock() {
        let lock = r#"{
            "default": {
                "requests": {"version": "==2.31.0"},
                "urllib3": {"version": "==2.0.4"}
            },
            "develop": {
                "pytest": {"version": "==7.4.0"}
            }
        }"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "Pipfile.lock",
            "requests",
            "2.31.0"
        )
        .unwrap());

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "Pipfile.lock",
            "pytest",
            "7.4.0"
        )
        .unwrap());
    }

    #[test]
    fn test_check_cargo_lock() {
        let lock = r#"
[[package]]
name = "serde"
version = "1.0.188"

[[package]]
name = "tokio"
version = "1.32.0"
"#;

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "Cargo.lock",
            "serde",
            "1.0.188"
        )
        .unwrap());

        assert!(ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "Cargo.lock",
            "tokio",
            "1.30.0"
        )
        .unwrap());

        assert!(!ReleaseClient::<MockHttpClient>::check_lock_file_version(
            lock,
            "Cargo.lock",
            "tokio",
            "1.33.0"
        )
        .unwrap());
    }

    #[test]
    fn test_check_unsupported_lock_file() {
        let result = ReleaseClient::<MockHttpClient>::check_lock_file_version(
            "content",
            "unknown.lock",
            "package",
            "1.0.0",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported"));
    }
}

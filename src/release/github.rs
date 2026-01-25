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
    pub async fn get_release_by_tag(
        &self,
        repo: &str,
        tag: &str,
    ) -> Result<Option<GitHubRelease>> {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::github::HttpResponse;

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
        let release = client.get_release_by_tag("org/repo", "v1.0.0").await.unwrap();

        assert!(release.is_some());
        assert_eq!(release.unwrap().tag_name, "v1.0.0");
    }

    #[tokio::test]
    async fn test_get_release_by_tag_not_found() {
        let mock = MockHttpClient::new(404, r#"{"message": "Not Found"}"#);

        let client = ReleaseClient::with_http_client("test-token", mock);
        let release = client.get_release_by_tag("org/repo", "v9.9.9").await.unwrap();

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
        let mock = MockHttpClient::new(
            200,
            r#"{"status": "ahead", "ahead_by": 3, "behind_by": 0}"#,
        );

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
        let mock = MockHttpClient::new(
            200,
            r#"{"merged": false, "merge_commit_sha": null}"#,
        );

        let client = ReleaseClient::with_http_client("test-token", mock);
        let sha = client.get_pr_merge_commit("org/repo", 42).await.unwrap();

        assert!(sha.is_none());
    }
}

//! GitLab API client for MR monitoring and issue management.

use crate::config::GitLabConfig;
use crate::error::{Error, Result};
use crate::http::HttpClient;
use crate::scm::{
    CodeReview, PrInfo, PrStatus, RemoteRepo, ReviewComment, ReviewUser, ScmProvider,
};
use async_trait::async_trait;
use serde::Deserialize;

/// GitLab API client for MR monitoring.
pub struct GitLabClient<H: HttpClient = crate::http::ReqwestHttpClient> {
    config: GitLabConfig,
    http: H,
}

// Internal deserialization structs for GitLab API responses
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitLabMergeRequest {
    iid: i64,
    state: String, // "opened", "closed", "merged"
    source_branch: Option<String>,
    target_branch: Option<String>,
    title: Option<String>,
    author: Option<GitLabUser>,
}

#[derive(Debug, Deserialize)]
pub struct GitLabUser {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GitLabNote {
    id: i64,
    body: String,
    author: GitLabUser,
    created_at: String,
    updated_at: String,
    #[serde(rename = "type")]
    note_type: Option<String>,
    system: bool,
    // For diff notes
    position: Option<GitLabNotePosition>,
}

#[derive(Debug, Deserialize)]
struct GitLabNotePosition {
    new_path: Option<String>,
    old_path: Option<String>,
    new_line: Option<i64>,
    old_line: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct GitLabApproval {
    user: GitLabUser,
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitLabApprovalsResponse {
    approved_by: Vec<GitLabApproval>,
}

#[derive(Debug, Deserialize)]
struct GitLabProject {
    id: i64,
    path_with_namespace: String,
    name: String,
    default_branch: Option<String>,
    http_url_to_repo: String,
    ssh_url_to_repo: String,
    web_url: String,
    visibility: Option<String>,
    archived: bool,
}

#[derive(Debug, Deserialize)]
struct GitLabDiffResponse {
    changes: Vec<GitLabDiffChange>,
}

#[derive(Debug, Deserialize)]
struct GitLabDiffChange {
    old_path: String,
    new_path: String,
    diff: String,
}

#[derive(Debug, Deserialize)]
pub struct GitLabIssue {
    pub iid: i64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    pub web_url: String,
    pub labels: Vec<String>,
    pub author: Option<GitLabUser>,
    pub assignees: Vec<GitLabUser>,
}

impl GitLabClient<crate::http::ReqwestHttpClient> {
    /// Create a new GitLab client with the default HTTP client.
    pub fn new(config: GitLabConfig) -> Self {
        Self {
            config,
            http: crate::http::ReqwestHttpClient::new(),
        }
    }
}

impl<H: HttpClient> GitLabClient<H> {
    /// Create a new GitLab client with a custom HTTP client.
    pub fn with_http_client(config: GitLabConfig, http: H) -> Self {
        Self { config, http }
    }

    /// Check if configured (has token).
    pub fn is_enabled(&self) -> bool {
        self.config.enabled && self.config.token.is_some()
    }

    /// Get the review trigger tag.
    pub fn review_trigger(&self) -> &str {
        &self.config.review_trigger
    }

    /// Get the GitLab API base URL.
    fn api_base(&self) -> &str {
        &self.config.base_url
    }

    /// URL-encode a project path for GitLab API calls.
    /// GitLab uses URL-encoded project paths like `group%2Fsubgroup%2Fproject`.
    fn encode_project_path(project: &str) -> String {
        urlencoding::encode(project).into_owned()
    }

    /// Build standard GitLab API headers.
    fn build_headers(&self, token: &str) -> Vec<(&'static str, String)> {
        vec![
            ("PRIVATE-TOKEN", token.to_string()),
            ("Accept", "application/json".to_string()),
            ("User-Agent", "claudear".to_string()),
        ]
    }

    /// Get the configured token.
    pub fn token(&self) -> Option<&str> {
        self.config.token.as_deref()
    }

    /// Get the webhook secret.
    pub fn webhook_secret(&self) -> Option<&str> {
        self.config.webhook_secret.as_deref()
    }

    /// Fetch project issues from a specific project.
    pub async fn get_project_issues(
        &self,
        project: &str,
        labels: &[String],
        state: Option<&str>,
    ) -> Result<Vec<GitLabIssue>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let mut url = format!(
            "{}/api/v4/projects/{}/issues?per_page=100",
            self.api_base(),
            encoded
        );

        if !labels.is_empty() {
            let encoded_labels: Vec<String> = labels
                .iter()
                .map(|l| urlencoding::encode(l).into_owned())
                .collect();
            url.push_str(&format!("&labels={}", encoded_labels.join(",")));
        }
        if let Some(s) = state {
            url.push_str(&format!("&state={}", urlencoding::encode(s)));
        }

        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitLab API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Fetch group issues.
    pub async fn get_group_issues(
        &self,
        group: &str,
        labels: &[String],
        state: Option<&str>,
    ) -> Result<Vec<GitLabIssue>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(group);
        let mut url = format!(
            "{}/api/v4/groups/{}/issues?per_page=100",
            self.api_base(),
            encoded
        );

        if !labels.is_empty() {
            let encoded_labels: Vec<String> = labels
                .iter()
                .map(|l| urlencoding::encode(l).into_owned())
                .collect();
            url.push_str(&format!("&labels={}", encoded_labels.join(",")));
        }
        if let Some(s) = state {
            url.push_str(&format!("&state={}", urlencoding::encode(s)));
        }

        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitLab API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Get a single issue.
    pub async fn get_issue(&self, project: &str, issue_iid: i64) -> Result<GitLabIssue> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let url = format!(
            "{}/api/v4/projects/{}/issues/{}",
            self.api_base(),
            encoded,
            issue_iid
        );

        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitLab API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Get MR notes (comments) for mapping to reviews.
    async fn get_mr_notes(&self, project: &str, mr_iid: i64) -> Result<Vec<GitLabNote>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let mut all_notes = Vec::new();
        let mut page = 1usize;
        const PER_PAGE: usize = 100;

        loop {
            let url = format!(
                "{}/api/v4/projects/{}/merge_requests/{}/notes?per_page={}&page={}&sort=asc",
                self.api_base(),
                encoded,
                mr_iid,
                PER_PAGE,
                page
            );

            let headers = self.build_headers(token);
            let response = self.http.get(&url, headers).await?;

            if !response.is_success() {
                return Err(Error::Other(format!(
                    "GitLab API error ({}): {}",
                    response.status, response.body
                )));
            }

            let notes: Vec<GitLabNote> = response.json()?;
            let count = notes.len();
            all_notes.extend(notes);

            if count < PER_PAGE {
                break;
            }

            page += 1;
            if page > 100 {
                tracing::warn!(project = %project, mr_iid, "Hit pagination limit for MR notes");
                break;
            }
        }

        Ok(all_notes)
    }

    /// Get MR approvals.
    async fn get_mr_approvals(
        &self,
        project: &str,
        mr_iid: i64,
    ) -> Result<GitLabApprovalsResponse> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/approvals",
            self.api_base(),
            encoded,
            mr_iid
        );

        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitLab API error ({}): {}",
                response.status, response.body
            )));
        }

        response.json()
    }

    /// Map a GitLab note to a CodeReview (for general notes).
    fn note_to_review(note: &GitLabNote) -> CodeReview {
        CodeReview {
            id: note.id,
            state: "COMMENTED".to_string(),
            body: Some(note.body.clone()),
            user: ReviewUser {
                id: note.author.id,
                login: note.author.username.clone(),
                user_type: None,
            },
            submitted_at: Some(note.created_at.clone()),
            html_url: None,
        }
    }

    /// Map a GitLab note to a ReviewComment (for diff notes).
    fn note_to_comment(note: &GitLabNote) -> ReviewComment {
        let (path, line, start_line) = if let Some(ref pos) = note.position {
            (
                pos.new_path
                    .clone()
                    .or(pos.old_path.clone())
                    .unwrap_or_default(),
                pos.new_line.or(pos.old_line),
                None,
            )
        } else {
            (String::new(), None, None)
        };

        ReviewComment {
            id: note.id,
            path,
            position: None,
            original_position: None,
            body: note.body.clone(),
            user: ReviewUser {
                id: note.author.id,
                login: note.author.username.clone(),
                user_type: None,
            },
            created_at: note.created_at.clone(),
            updated_at: note.updated_at.clone(),
            html_url: String::new(),
            pull_request_review_id: None,
            line,
            start_line,
            side: None,
        }
    }
}

#[async_trait]
impl<H: HttpClient> ScmProvider for GitLabClient<H> {
    fn name(&self) -> &str {
        "gitlab"
    }

    fn is_enabled(&self) -> bool {
        self.is_enabled()
    }

    fn review_trigger(&self) -> &str {
        self.review_trigger()
    }

    async fn get_pr_status(&self, project: &str, number: i64) -> Result<PrStatus> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            self.api_base(),
            encoded,
            number
        );
        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if response.is_not_found() {
            return Err(Error::Other(format!(
                "MR not found: {}!{}",
                project, number
            )));
        }

        if !response.is_success() {
            return Err(Error::Other(format!("GitLab API error: {}", response.body)));
        }

        let mr: GitLabMergeRequest = response.json()?;

        Ok(match mr.state.as_str() {
            "merged" => PrStatus::Merged,
            "closed" => PrStatus::Closed,
            _ => PrStatus::Open,
        })
    }

    async fn get_pr_info(&self, project: &str, number: i64) -> Result<PrInfo> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            self.api_base(),
            encoded,
            number
        );
        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!("GitLab API error: {}", response.body)));
        }

        let mr: GitLabMergeRequest = response.json()?;

        Ok(PrInfo {
            head_branch: mr.source_branch,
            base_branch: mr.target_branch,
            title: mr.title,
            author: mr.author.map(|a| a.username),
        })
    }

    async fn get_pr_diff(&self, project: &str, number: i64) -> Result<String> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(project);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/changes",
            self.api_base(),
            encoded,
            number
        );
        let headers = self.build_headers(token);
        let response = self.http.get(&url, headers).await?;

        if !response.is_success() {
            return Err(Error::Other(format!(
                "GitLab API error fetching diff for {}!{}: {}",
                project, number, response.body
            )));
        }

        // Parse the changes response and format as unified diff
        let diff_resp: GitLabDiffResponse = response.json()?;
        let mut unified_diff = String::new();
        for change in diff_resp.changes {
            unified_diff.push_str(&format!(
                "--- a/{}\n+++ b/{}\n",
                change.old_path, change.new_path
            ));
            unified_diff.push_str(&change.diff);
            unified_diff.push('\n');
        }

        Ok(unified_diff)
    }

    async fn get_reviews(&self, project: &str, number: i64) -> Result<Vec<CodeReview>> {
        let notes = self.get_mr_notes(project, number).await?;
        let approvals =
            self.get_mr_approvals(project, number)
                .await
                .unwrap_or(GitLabApprovalsResponse {
                    approved_by: vec![],
                });

        let mut reviews = Vec::new();

        // Map general notes (non-diff, non-system) to reviews
        for note in &notes {
            if note.system {
                continue;
            }
            if note.position.is_some() {
                continue; // Diff notes go to review comments
            }
            reviews.push(Self::note_to_review(note));
        }

        // Map approvals to reviews (use negative user ID to avoid collision with note IDs)
        for approval in &approvals.approved_by {
            reviews.push(CodeReview {
                id: -(approval.user.id),
                state: "APPROVED".to_string(),
                body: None,
                user: ReviewUser {
                    id: approval.user.id,
                    login: approval.user.username.clone(),
                    user_type: None,
                },
                submitted_at: approval.created_at.clone(),
                html_url: None,
            });
        }

        Ok(reviews)
    }

    async fn get_review_comments(&self, project: &str, number: i64) -> Result<Vec<ReviewComment>> {
        let notes = self.get_mr_notes(project, number).await?;

        let mut comments = Vec::new();

        for note in &notes {
            if note.system {
                continue;
            }
            if note.position.is_none() {
                continue; // General notes go to reviews
            }
            comments.push(Self::note_to_comment(note));
        }

        Ok(comments)
    }

    async fn list_repos(&self, group: &str) -> Result<Vec<RemoteRepo>> {
        let token = self
            .config
            .token
            .as_ref()
            .ok_or_else(|| Error::config("GitLab token not configured"))?;

        let encoded = Self::encode_project_path(group);
        let mut all_repos = Vec::new();
        let mut page = 1;
        const PER_PAGE: usize = 100;

        loop {
            let url = format!(
                "{}/api/v4/groups/{}/projects?per_page={}&page={}&include_subgroups=true",
                self.api_base(),
                encoded,
                PER_PAGE,
                page
            );
            let headers = self.build_headers(token);
            let response = self.http.get(&url, headers).await?;

            if response.is_not_found() {
                return Err(Error::Other(format!("Group not found: {}", group)));
            }

            if !response.is_success() {
                return Err(Error::Other(format!(
                    "GitLab API error ({}): {}",
                    response.status, response.body
                )));
            }

            let projects: Vec<GitLabProject> = response.json()?;
            let count = projects.len();

            let active_repos: Vec<RemoteRepo> = projects
                .into_iter()
                .filter(|p| !p.archived)
                .map(|p| RemoteRepo {
                    id: p.id,
                    full_name: p.path_with_namespace,
                    name: p.name,
                    default_branch: p.default_branch.unwrap_or_else(|| "main".to_string()),
                    clone_url: p.http_url_to_repo,
                    ssh_url: p.ssh_url_to_repo,
                    html_url: p.web_url,
                    private: p.visibility.as_deref() != Some("public"),
                    archived: false,
                })
                .collect();
            all_repos.extend(active_repos);

            if count < PER_PAGE {
                break;
            }

            page += 1;
            if page > 100 {
                tracing::warn!(group = %group, "Hit pagination limit for group projects");
                break;
            }
        }

        tracing::info!(group = %group, count = all_repos.len(), "Fetched group projects");
        Ok(all_repos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GitLabConfig;
    use crate::http::HttpResponse;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing GitLab API calls.
    struct MockHttpClient {
        responses: Mutex<HashMap<String, HttpResponse>>,
        /// Captured headers from the most recent request, keyed by URL.
        captured_headers: Mutex<HashMap<String, Vec<(String, String)>>>,
    }

    impl MockHttpClient {
        fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                captured_headers: Mutex::new(HashMap::new()),
            }
        }

        /// Add a mock response for a URL.
        fn mock_response(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            self.responses.lock().unwrap().insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        /// Get captured headers for a given URL.
        fn get_captured_headers(&self, url: &str) -> Option<Vec<(String, String)>> {
            self.captured_headers.lock().unwrap().get(url).cloned()
        }
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn get(&self, url: &str, headers: Vec<(&str, String)>) -> Result<HttpResponse> {
            // Capture headers for inspection
            {
                let owned: Vec<(String, String)> = headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.clone()))
                    .collect();
                self.captured_headers
                    .lock()
                    .unwrap()
                    .insert(url.to_string(), owned);
            }

            let responses = self.responses.lock().unwrap();
            if let Some(response) = responses.get(url) {
                Ok(HttpResponse {
                    status: response.status,
                    body: response.body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not found".to_string(),
                })
            }
        }
    }

    fn test_config() -> GitLabConfig {
        let mut config = GitLabConfig::test_default();
        config.groups = vec![];
        config.trigger_labels = vec![];
        config.trigger_states = vec![];
        config
    }

    #[tokio::test]
    async fn test_get_mr_status_open() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "opened"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let status = client.get_pr_status("group/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Open);
    }

    #[tokio::test]
    async fn test_get_mr_status_merged() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "merged"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let status = client.get_pr_status("group/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Merged);
    }

    #[tokio::test]
    async fn test_get_mr_status_closed() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "closed"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let status = client.get_pr_status("group/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Closed);
    }

    #[tokio::test]
    async fn test_get_mr_info() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "opened", "source_branch": "feature", "target_branch": "main", "title": "Add feature", "author": {"id": 1, "username": "dev"}}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let info = client.get_pr_info("group/repo", 1).await.unwrap();
        assert_eq!(info.head_branch, Some("feature".to_string()));
        assert_eq!(info.base_branch, Some("main".to_string()));
        assert_eq!(info.title, Some("Add feature".to_string()));
        assert_eq!(info.author, Some("dev".to_string()));
    }

    #[test]
    fn test_encode_project_path() {
        assert_eq!(
            GitLabClient::<crate::http::ReqwestHttpClient>::encode_project_path("group/repo"),
            "group%2Frepo"
        );
        assert_eq!(
            GitLabClient::<crate::http::ReqwestHttpClient>::encode_project_path(
                "group/subgroup/repo"
            ),
            "group%2Fsubgroup%2Frepo"
        );
    }

    #[tokio::test]
    async fn test_list_repos() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 1,
                "path_with_namespace": "mygroup/repo1",
                "name": "repo1",
                "default_branch": "main",
                "http_url_to_repo": "https://gitlab.com/mygroup/repo1.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/repo1.git",
                "web_url": "https://gitlab.com/mygroup/repo1",
                "visibility": "private",
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 1);
        assert_eq!(repos[0].full_name, "mygroup/repo1");
        assert!(repos[0].private);
    }

    #[test]
    fn test_is_enabled_with_token() {
        let config = test_config();
        let client = GitLabClient::new(config);
        assert!(client.is_enabled());
    }

    #[test]
    fn test_is_enabled_without_token() {
        let mut config = test_config();
        config.token = None;
        let client = GitLabClient::new(config);
        assert!(!client.is_enabled());
    }

    #[test]
    fn test_is_enabled_disabled() {
        let mut config = test_config();
        config.enabled = false;
        let client = GitLabClient::new(config);
        assert!(!client.is_enabled());
    }

    // ── Token-missing error path tests ──────────────────────────────────────

    fn no_token_config() -> GitLabConfig {
        GitLabConfig {
            token: None,
            ..GitLabConfig::test_default()
        }
    }

    #[tokio::test]
    async fn test_get_pr_status_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_pr_status("group/project", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_info_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_pr_info("group/project", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_diff_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_pr_diff("group/project", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_reviews_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_reviews("group/project", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_review_comments_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_review_comments("group/project", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_list_repos_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.list_repos("mygroup").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    // ── API error response tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_status_500() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_status("group/repo", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Internal Server Error") || err_msg.contains("API error"),
            "error should contain API error info: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_status_404_mr_not_found() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/999",
            404,
            "Not found",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_status("group/repo", 999).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("MR not found"),
            "error should say MR not found: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_info_500() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_info("group/repo", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Internal Server Error") || err_msg.contains("API error"),
            "error should contain API error info: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_list_repos_404() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/nonexistent/projects?per_page=100&page=1&include_subgroups=true",
            404,
            "Group not found",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.list_repos("nonexistent").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("not found") || err_msg.contains("Group not found"),
            "error should mention not found: {err_msg}"
        );
    }

    // ── get_reviews with notes and approvals ────────────────────────────────

    #[tokio::test]
    async fn test_get_reviews_with_notes_and_approvals() {
        let mock = MockHttpClient::new();

        // Notes endpoint (page 1 -- fewer than PER_PAGE so pagination stops)
        let notes_json = r#"[
            {
                "id": 101,
                "body": "Looks good overall",
                "author": { "id": 10, "username": "reviewer_a" },
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T01:00:00Z",
                "type": null,
                "system": false,
                "position": null
            },
            {
                "id": 102,
                "body": "System note: merged",
                "author": { "id": 10, "username": "reviewer_a" },
                "created_at": "2025-01-02T00:00:00Z",
                "updated_at": "2025-01-02T00:00:00Z",
                "type": null,
                "system": true,
                "position": null
            },
            {
                "id": 103,
                "body": "Diff-level remark",
                "author": { "id": 20, "username": "reviewer_b" },
                "created_at": "2025-01-03T00:00:00Z",
                "updated_at": "2025-01-03T01:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {
                    "new_path": "src/main.rs",
                    "old_path": "src/main.rs",
                    "new_line": 42,
                    "old_line": null
                }
            }
        ]"#;
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            notes_json,
        );

        // Approvals endpoint
        let approvals_json = r#"{
            "approved_by": [
                {
                    "user": { "id": 30, "username": "approver_c" },
                    "created_at": "2025-01-04T00:00:00Z"
                }
            ]
        }"#;
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            approvals_json,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();

        // Should include: note 101 (general comment), skip note 102 (system),
        // skip note 103 (diff note), plus 1 approval = 2 total
        assert_eq!(
            reviews.len(),
            2,
            "expected 2 reviews, got {}",
            reviews.len()
        );

        // First review: the general note
        assert_eq!(reviews[0].id, 101);
        assert_eq!(reviews[0].state, "COMMENTED");
        assert_eq!(reviews[0].body.as_deref(), Some("Looks good overall"));
        assert_eq!(reviews[0].user.login, "reviewer_a");
        assert_eq!(reviews[0].user.id, 10);
        assert_eq!(
            reviews[0].submitted_at.as_deref(),
            Some("2025-01-01T00:00:00Z")
        );

        // Second review: the approval (negative user ID to avoid collision with note IDs)
        assert_eq!(reviews[1].id, -30);
        assert_eq!(reviews[1].state, "APPROVED");
        assert!(reviews[1].body.is_none());
        assert_eq!(reviews[1].user.login, "approver_c");
        assert_eq!(reviews[1].user.id, 30);
        assert_eq!(
            reviews[1].submitted_at.as_deref(),
            Some("2025-01-04T00:00:00Z")
        );
    }

    // ── get_review_comments with diff notes ─────────────────────────────────

    #[tokio::test]
    async fn test_get_review_comments_with_diff_notes() {
        let mock = MockHttpClient::new();

        let notes_json = r#"[
            {
                "id": 201,
                "body": "This is a general comment, not a diff note",
                "author": { "id": 10, "username": "reviewer_a" },
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T01:00:00Z",
                "type": null,
                "system": false,
                "position": null
            },
            {
                "id": 202,
                "body": "Fix this line",
                "author": { "id": 20, "username": "reviewer_b" },
                "created_at": "2025-02-01T00:00:00Z",
                "updated_at": "2025-02-01T01:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {
                    "new_path": "src/lib.rs",
                    "old_path": "src/lib.rs",
                    "new_line": 15,
                    "old_line": 12
                }
            },
            {
                "id": 203,
                "body": "Added in new file",
                "author": { "id": 30, "username": "reviewer_c" },
                "created_at": "2025-03-01T00:00:00Z",
                "updated_at": "2025-03-01T01:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {
                    "new_path": "src/new_module.rs",
                    "old_path": null,
                    "new_line": 5,
                    "old_line": null
                }
            },
            {
                "id": 204,
                "body": "System auto-merge",
                "author": { "id": 1, "username": "system" },
                "created_at": "2025-03-02T00:00:00Z",
                "updated_at": "2025-03-02T00:00:00Z",
                "type": null,
                "system": true,
                "position": null
            }
        ]"#;
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/5/notes?per_page=100&page=1&sort=asc",
            200,
            notes_json,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client.get_review_comments("group/repo", 5).await.unwrap();

        // Only diff notes (non-system, with position) should be returned:
        // note 202 and 203; note 201 has no position, note 204 is system.
        assert_eq!(
            comments.len(),
            2,
            "expected 2 diff-note comments, got {}",
            comments.len()
        );

        // First diff comment
        assert_eq!(comments[0].id, 202);
        assert_eq!(comments[0].body, "Fix this line");
        assert_eq!(comments[0].path, "src/lib.rs");
        assert_eq!(comments[0].line, Some(15)); // new_line preferred
        assert_eq!(comments[0].user.login, "reviewer_b");
        assert_eq!(comments[0].user.id, 20);
        assert_eq!(comments[0].created_at, "2025-02-01T00:00:00Z");
        assert_eq!(comments[0].updated_at, "2025-02-01T01:00:00Z");

        // Second diff comment (new file, no old_path)
        assert_eq!(comments[1].id, 203);
        assert_eq!(comments[1].body, "Added in new file");
        assert_eq!(comments[1].path, "src/new_module.rs");
        assert_eq!(comments[1].line, Some(5));
        assert_eq!(comments[1].user.login, "reviewer_c");
    }

    // ── get_pr_diff success path ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_diff_success() {
        let mock = MockHttpClient::new();

        let changes_json = r#"{
            "changes": [
                {
                    "old_path": "src/main.rs",
                    "new_path": "src/main.rs",
                    "diff": "@@ -1,3 +1,4 @@\n fn main() {\n+    println!(\"hello\");\n }\n"
                },
                {
                    "old_path": "README.md",
                    "new_path": "README.md",
                    "diff": "@@ -1 +1,2 @@\n # Project\n+Some docs\n"
                }
            ]
        }"#;
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/7/changes",
            200,
            changes_json,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let diff = client.get_pr_diff("group/repo", 7).await.unwrap();

        // Verify unified diff header for first change
        assert!(
            diff.contains("--- a/src/main.rs"),
            "diff should contain old path header"
        );
        assert!(
            diff.contains("+++ b/src/main.rs"),
            "diff should contain new path header"
        );
        assert!(
            diff.contains("println!(\"hello\")"),
            "diff should contain the actual change"
        );

        // Verify unified diff header for second change
        assert!(
            diff.contains("--- a/README.md"),
            "diff should contain README old path header"
        );
        assert!(
            diff.contains("+++ b/README.md"),
            "diff should contain README new path header"
        );
        assert!(
            diff.contains("Some docs"),
            "diff should contain README change"
        );
    }

    // ── encode_project_path additional tests ──────────────────────────────

    #[test]
    fn test_encode_project_path_no_slashes() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("simple-project"),
            "simple-project"
        );
    }

    #[test]
    fn test_encode_project_path_deeply_nested() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("a/b/c/d/e"),
            "a%2Fb%2Fc%2Fd%2Fe"
        );
    }

    #[test]
    fn test_encode_project_path_empty() {
        assert_eq!(GitLabClient::<MockHttpClient>::encode_project_path(""), "");
    }

    #[test]
    fn test_encode_project_path_trailing_slash() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("group/"),
            "group%2F"
        );
    }

    #[test]
    fn test_encode_project_path_leading_slash() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("/repo"),
            "%2Frepo"
        );
    }

    #[test]
    fn test_encode_project_path_consecutive_slashes() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("a//b"),
            "a%2F%2Fb"
        );
    }

    #[test]
    fn test_encode_project_path_with_hyphens_and_dots() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("my-group/my.project"),
            "my-group%2Fmy.project"
        );
    }

    // ── get_pr_status additional tests ────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_status_unknown_state_maps_to_open() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "some_unknown_state"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let status = client.get_pr_status("group/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Open);
    }

    #[tokio::test]
    async fn test_get_pr_status_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            "this is not json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_status("group/repo", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("JSON") || err_msg.contains("parse"),
            "error should mention JSON parse: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_status_nested_project_path() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/org%2Fteam%2Frepo/merge_requests/42",
            200,
            r#"{"iid": 42, "state": "opened"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let status = client.get_pr_status("org/team/repo", 42).await.unwrap();
        assert_eq!(status, PrStatus::Open);
    }

    #[tokio::test]
    async fn test_get_pr_status_401_unauthorized() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            401,
            r#"{"message":"401 Unauthorized"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_status("group/repo", 1).await;
        assert!(result.is_err());
    }

    // ── get_pr_info additional tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_info_optional_fields_missing() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/2",
            200,
            r#"{"iid": 2, "state": "opened"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let info = client.get_pr_info("group/repo", 2).await.unwrap();
        assert!(info.head_branch.is_none());
        assert!(info.base_branch.is_none());
        assert!(info.title.is_none());
        assert!(info.author.is_none());
    }

    #[tokio::test]
    async fn test_get_pr_info_partial_fields() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/3",
            200,
            r#"{"iid": 3, "state": "merged", "source_branch": "fix-bug", "title": "Fix critical bug"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let info = client.get_pr_info("group/repo", 3).await.unwrap();
        assert_eq!(info.head_branch, Some("fix-bug".to_string()));
        assert!(info.base_branch.is_none());
        assert_eq!(info.title, Some("Fix critical bug".to_string()));
        assert!(info.author.is_none());
    }

    #[tokio::test]
    async fn test_get_pr_info_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            503,
            "Service Unavailable",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_info("group/repo", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Service Unavailable") || err_msg.contains("API error"),
            "error should contain API error info: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_info_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            "{broken json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_info("group/repo", 1).await;
        assert!(result.is_err());
    }

    // ── get_pr_diff additional tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_get_pr_diff_empty_changes() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/10/changes",
            200,
            r#"{"changes": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let diff = client.get_pr_diff("group/repo", 10).await.unwrap();
        assert!(diff.is_empty(), "empty changes should produce empty diff");
    }

    #[tokio::test]
    async fn test_get_pr_diff_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/10/changes",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_diff("group/repo", 10).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Internal Server Error"),
            "error should mention the API failure: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_pr_diff_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/10/changes",
            200,
            "not json at all",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_diff("group/repo", 10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_pr_diff_single_change() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/11/changes",
            200,
            r#"{"changes": [{"old_path": "a.rs", "new_path": "b.rs", "diff": "@@ -1 +1 @@\n-old\n+new\n"}]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let diff = client.get_pr_diff("group/repo", 11).await.unwrap();
        assert!(diff.contains("--- a/a.rs"));
        assert!(diff.contains("+++ b/b.rs"));
        assert!(diff.contains("-old"));
        assert!(diff.contains("+new"));
    }

    #[tokio::test]
    async fn test_get_pr_diff_rename() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/12/changes",
            200,
            r#"{"changes": [{"old_path": "old_name.rs", "new_path": "new_name.rs", "diff": ""}]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let diff = client.get_pr_diff("group/repo", 12).await.unwrap();
        assert!(diff.contains("--- a/old_name.rs"));
        assert!(diff.contains("+++ b/new_name.rs"));
    }

    // ── get_reviews additional tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_get_reviews_empty_notes_and_no_approvals() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "[]",
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert!(reviews.is_empty());
    }

    #[tokio::test]
    async fn test_get_reviews_only_system_notes_filtered_out() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "merged",
                "author": {"id": 1, "username": "bot"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": null,
                "system": true,
                "position": null
            }]"#,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert!(reviews.is_empty(), "system notes should be filtered out");
    }

    #[tokio::test]
    async fn test_get_reviews_only_diff_notes_excluded() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "inline comment",
                "author": {"id": 1, "username": "reviewer"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {"new_path": "main.rs", "old_path": "main.rs", "new_line": 1, "old_line": null}
            }]"#,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert!(
            reviews.is_empty(),
            "diff notes should not appear in reviews"
        );
    }

    #[tokio::test]
    async fn test_get_reviews_approvals_api_failure_gracefully_handled() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 50,
                "body": "Nice work",
                "author": {"id": 5, "username": "reviewer"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": null,
                "system": false,
                "position": null
            }]"#,
        );
        // Approvals endpoint returns 500 - should be handled gracefully
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        // Should still return the note-based review even though approvals failed
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].id, 50);
        assert_eq!(reviews[0].state, "COMMENTED");
    }

    #[tokio::test]
    async fn test_get_reviews_multiple_approvals() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "[]",
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": [
                {"user": {"id": 10, "username": "alice"}, "created_at": "2025-01-01T00:00:00Z"},
                {"user": {"id": 20, "username": "bob"}, "created_at": "2025-01-02T00:00:00Z"}
            ]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert_eq!(reviews.len(), 2);
        assert_eq!(reviews[0].state, "APPROVED");
        assert_eq!(reviews[0].user.login, "alice");
        assert_eq!(reviews[1].state, "APPROVED");
        assert_eq!(reviews[1].user.login, "bob");
    }

    #[tokio::test]
    async fn test_get_reviews_approval_without_created_at() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "[]",
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": [
                {"user": {"id": 10, "username": "alice"}, "created_at": null}
            ]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].state, "APPROVED");
        assert!(reviews[0].submitted_at.is_none());
    }

    // ── get_review_comments additional tests ──────────────────────────────

    #[tokio::test]
    async fn test_get_review_comments_empty() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client.get_review_comments("group/repo", 1).await.unwrap();
        assert!(comments.is_empty());
    }

    #[tokio::test]
    async fn test_get_review_comments_only_general_notes_excluded() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "general note",
                "author": {"id": 1, "username": "reviewer"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": null,
                "system": false,
                "position": null
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client.get_review_comments("group/repo", 1).await.unwrap();
        assert!(
            comments.is_empty(),
            "general notes should not appear in review comments"
        );
    }

    #[tokio::test]
    async fn test_get_review_comments_position_fallback_to_old_path() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 301,
                "body": "deleted line comment",
                "author": {"id": 5, "username": "reviewer"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {
                    "new_path": null,
                    "old_path": "deleted_file.rs",
                    "new_line": null,
                    "old_line": 10
                }
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client.get_review_comments("group/repo", 1).await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].path, "deleted_file.rs");
        assert_eq!(comments[0].line, Some(10));
    }

    #[tokio::test]
    async fn test_get_review_comments_system_diff_note_excluded() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "system diff note",
                "author": {"id": 1, "username": "system"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": "DiffNote",
                "system": true,
                "position": {"new_path": "file.rs", "old_path": "file.rs", "new_line": 1, "old_line": null}
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client.get_review_comments("group/repo", 1).await.unwrap();
        assert!(
            comments.is_empty(),
            "system notes should be excluded even if they have a position"
        );
    }

    // ── get_new_reviews / get_new_review_comments tests ───────────────────

    #[tokio::test]
    async fn test_get_new_reviews_no_since_returns_all() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "comment",
                "author": {"id": 1, "username": "dev"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": null,
                "system": false,
                "position": null
            }]"#,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_new_reviews("group/repo", 1, None).await.unwrap();
        assert_eq!(reviews.len(), 1);
    }

    #[tokio::test]
    async fn test_get_new_reviews_with_since_filters() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[
                {
                    "id": 1,
                    "body": "old comment",
                    "author": {"id": 1, "username": "dev"},
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "type": null,
                    "system": false,
                    "position": null
                },
                {
                    "id": 2,
                    "body": "new comment",
                    "author": {"id": 2, "username": "dev2"},
                    "created_at": "2025-06-01T00:00:00Z",
                    "updated_at": "2025-06-01T00:00:00Z",
                    "type": null,
                    "system": false,
                    "position": null
                }
            ]"#,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client
            .get_new_reviews("group/repo", 1, Some("2025-03-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].id, 2);
        assert_eq!(reviews[0].body.as_deref(), Some("new comment"));
    }

    #[tokio::test]
    async fn test_get_new_reviews_since_equal_timestamp_included() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "exact match",
                "author": {"id": 1, "username": "dev"},
                "created_at": "2025-06-01T00:00:00Z",
                "updated_at": "2025-06-01T00:00:00Z",
                "type": null,
                "system": false,
                "position": null
            }]"#,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client
            .get_new_reviews("group/repo", 1, Some("2025-06-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(
            reviews.len(),
            1,
            "review at exact 'since' timestamp should be included"
        );
    }

    #[tokio::test]
    async fn test_get_new_review_comments_no_since_returns_all() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "diff comment",
                "author": {"id": 1, "username": "dev"},
                "created_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-01T00:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {"new_path": "main.rs", "old_path": "main.rs", "new_line": 5, "old_line": null}
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client
            .get_new_review_comments("group/repo", 1, None)
            .await
            .unwrap();
        assert_eq!(comments.len(), 1);
    }

    #[tokio::test]
    async fn test_get_new_review_comments_with_since_filters_by_updated_at() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[
                {
                    "id": 1,
                    "body": "old diff comment",
                    "author": {"id": 1, "username": "dev"},
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "type": "DiffNote",
                    "system": false,
                    "position": {"new_path": "a.rs", "old_path": "a.rs", "new_line": 1, "old_line": null}
                },
                {
                    "id": 2,
                    "body": "new diff comment",
                    "author": {"id": 2, "username": "dev2"},
                    "created_at": "2025-06-01T00:00:00Z",
                    "updated_at": "2025-06-15T00:00:00Z",
                    "type": "DiffNote",
                    "system": false,
                    "position": {"new_path": "b.rs", "old_path": "b.rs", "new_line": 10, "old_line": null}
                }
            ]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client
            .get_new_review_comments("group/repo", 1, Some("2025-03-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, 2);
    }

    // ── list_repos additional tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_list_repos_empty_group() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/emptygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("emptygroup").await.unwrap();
        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn test_list_repos_filters_archived() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[
                {
                    "id": 1,
                    "path_with_namespace": "mygroup/active",
                    "name": "active",
                    "default_branch": "main",
                    "http_url_to_repo": "https://gitlab.com/mygroup/active.git",
                    "ssh_url_to_repo": "git@gitlab.com:mygroup/active.git",
                    "web_url": "https://gitlab.com/mygroup/active",
                    "visibility": "private",
                    "archived": false
                },
                {
                    "id": 2,
                    "path_with_namespace": "mygroup/archived-repo",
                    "name": "archived-repo",
                    "default_branch": "main",
                    "http_url_to_repo": "https://gitlab.com/mygroup/archived-repo.git",
                    "ssh_url_to_repo": "git@gitlab.com:mygroup/archived-repo.git",
                    "web_url": "https://gitlab.com/mygroup/archived-repo",
                    "visibility": "private",
                    "archived": true
                }
            ]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 1, "archived repos should be filtered out");
        assert_eq!(repos[0].name, "active");
        assert!(!repos[0].archived);
    }

    #[tokio::test]
    async fn test_list_repos_visibility_public_not_private() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 1,
                "path_with_namespace": "mygroup/public-repo",
                "name": "public-repo",
                "default_branch": "main",
                "http_url_to_repo": "https://gitlab.com/mygroup/public-repo.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/public-repo.git",
                "web_url": "https://gitlab.com/mygroup/public-repo",
                "visibility": "public",
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 1);
        assert!(
            !repos[0].private,
            "public visibility should map to private=false"
        );
    }

    #[tokio::test]
    async fn test_list_repos_visibility_internal_is_private() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 1,
                "path_with_namespace": "mygroup/internal-repo",
                "name": "internal-repo",
                "default_branch": "develop",
                "http_url_to_repo": "https://gitlab.com/mygroup/internal-repo.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/internal-repo.git",
                "web_url": "https://gitlab.com/mygroup/internal-repo",
                "visibility": "internal",
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 1);
        assert!(
            repos[0].private,
            "internal visibility should map to private=true"
        );
    }

    #[tokio::test]
    async fn test_list_repos_no_default_branch_defaults_to_main() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 1,
                "path_with_namespace": "mygroup/no-default",
                "name": "no-default",
                "default_branch": null,
                "http_url_to_repo": "https://gitlab.com/mygroup/no-default.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/no-default.git",
                "web_url": "https://gitlab.com/mygroup/no-default",
                "visibility": "private",
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos[0].default_branch, "main");
    }

    #[tokio::test]
    async fn test_list_repos_visibility_none_is_private() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 1,
                "path_with_namespace": "mygroup/no-vis",
                "name": "no-vis",
                "default_branch": "main",
                "http_url_to_repo": "https://gitlab.com/mygroup/no-vis.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/no-vis.git",
                "web_url": "https://gitlab.com/mygroup/no-vis",
                "visibility": null,
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert!(
            repos[0].private,
            "null visibility should map to private=true"
        );
    }

    #[tokio::test]
    async fn test_list_repos_field_mapping() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            r#"[{
                "id": 42,
                "path_with_namespace": "mygroup/myrepo",
                "name": "myrepo",
                "default_branch": "develop",
                "http_url_to_repo": "https://gitlab.com/mygroup/myrepo.git",
                "ssh_url_to_repo": "git@gitlab.com:mygroup/myrepo.git",
                "web_url": "https://gitlab.com/mygroup/myrepo",
                "visibility": "private",
                "archived": false
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 1);
        let repo = &repos[0];
        assert_eq!(repo.id, 42);
        assert_eq!(repo.full_name, "mygroup/myrepo");
        assert_eq!(repo.name, "myrepo");
        assert_eq!(repo.default_branch, "develop");
        assert_eq!(repo.clone_url, "https://gitlab.com/mygroup/myrepo.git");
        assert_eq!(repo.ssh_url, "git@gitlab.com:mygroup/myrepo.git");
        assert_eq!(repo.html_url, "https://gitlab.com/mygroup/myrepo");
        assert!(repo.private);
        assert!(!repo.archived);
    }

    #[tokio::test]
    async fn test_list_repos_500_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.list_repos("mygroup").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_repos_nested_group_path_encoded() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/org%2Fsubgroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("org/subgroup").await.unwrap();
        assert!(repos.is_empty());
    }

    // ── Issue operations tests ────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_project_issues_success() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100",
            200,
            r#"[{
                "iid": 1,
                "title": "Bug report",
                "description": "Something is broken",
                "state": "opened",
                "web_url": "https://gitlab.com/group/repo/-/issues/1",
                "labels": ["bug", "priority::high"],
                "author": {"id": 10, "username": "reporter"},
                "assignees": [{"id": 20, "username": "dev"}]
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let issues = client
            .get_project_issues("group/repo", &[], None)
            .await
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].iid, 1);
        assert_eq!(issues[0].title, "Bug report");
        assert_eq!(
            issues[0].description.as_deref(),
            Some("Something is broken")
        );
        assert_eq!(issues[0].state, "opened");
        assert_eq!(
            issues[0].web_url,
            "https://gitlab.com/group/repo/-/issues/1"
        );
        assert_eq!(issues[0].labels, vec!["bug", "priority::high"]);
        assert_eq!(issues[0].author.as_ref().unwrap().username, "reporter");
        assert_eq!(issues[0].assignees[0].username, "dev");
    }

    #[tokio::test]
    async fn test_get_project_issues_with_labels_and_state() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100&labels=bug,critical&state=opened",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let labels = vec!["bug".to_string(), "critical".to_string()];
        let issues = client
            .get_project_issues("group/repo", &labels, Some("opened"))
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn test_get_project_issues_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100",
            403,
            "Forbidden",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_project_issues("group/repo", &[], None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("403") || err_msg.contains("Forbidden"),
            "error should contain status or body: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_project_issues_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_project_issues("group/repo", &[], None).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("token"),
            "error should mention token: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_get_group_issues_success() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/issues?per_page=100&labels=auto-implement&state=opened",
            200,
            r#"[{
                "iid": 5,
                "title": "Feature request",
                "description": null,
                "state": "opened",
                "web_url": "https://gitlab.com/mygroup/repo/-/issues/5",
                "labels": ["auto-implement"],
                "author": null,
                "assignees": []
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let labels = vec!["auto-implement".to_string()];
        let issues = client
            .get_group_issues("mygroup", &labels, Some("opened"))
            .await
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].iid, 5);
        assert!(issues[0].description.is_none());
        assert!(issues[0].author.is_none());
        assert!(issues[0].assignees.is_empty());
    }

    #[tokio::test]
    async fn test_get_group_issues_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_group_issues("mygroup", &[], None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_group_issues_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/issues?per_page=100",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_group_issues("mygroup", &[], None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_issue_success() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues/42",
            200,
            r#"{
                "iid": 42,
                "title": "Single issue",
                "description": "Details here",
                "state": "opened",
                "web_url": "https://gitlab.com/group/repo/-/issues/42",
                "labels": [],
                "author": {"id": 1, "username": "admin"},
                "assignees": []
            }"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let issue = client.get_issue("group/repo", 42).await.unwrap();
        assert_eq!(issue.iid, 42);
        assert_eq!(issue.title, "Single issue");
    }

    #[tokio::test]
    async fn test_get_issue_not_found() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues/999",
            404,
            "Not found",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_issue("group/repo", 999).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_issue_no_token() {
        let client = GitLabClient::with_http_client(no_token_config(), MockHttpClient::new());
        let result = client.get_issue("group/repo", 1).await;
        assert!(result.is_err());
    }

    // ── Authentication header tests ───────────────────────────────────────

    #[tokio::test]
    async fn test_auth_headers_private_token_format() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "opened"}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let _ = client.get_pr_status("group/repo", 1).await.unwrap();

        let headers = client
            .http
            .get_captured_headers(
                "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            )
            .unwrap();

        // Check PRIVATE-TOKEN header
        let token_header = headers
            .iter()
            .find(|(k, _)| k == "PRIVATE-TOKEN")
            .expect("should have PRIVATE-TOKEN header");
        assert_eq!(token_header.1, "test_token");

        // Check Accept header
        let accept_header = headers
            .iter()
            .find(|(k, _)| k == "Accept")
            .expect("should have Accept header");
        assert_eq!(accept_header.1, "application/json");

        // Check User-Agent header
        let ua_header = headers
            .iter()
            .find(|(k, _)| k == "User-Agent")
            .expect("should have User-Agent header");
        assert_eq!(ua_header.1, "claudear");
    }

    #[tokio::test]
    async fn test_auth_headers_custom_token() {
        let mut config = test_config();
        config.token = Some("my-custom-gl-token".to_string());

        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "opened"}"#,
        );

        let client = GitLabClient::with_http_client(config, mock);
        let _ = client.get_pr_status("group/repo", 1).await.unwrap();

        let headers = client
            .http
            .get_captured_headers(
                "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            )
            .unwrap();

        let token_header = headers
            .iter()
            .find(|(k, _)| k == "PRIVATE-TOKEN")
            .expect("should have PRIVATE-TOKEN header");
        assert_eq!(token_header.1, "my-custom-gl-token");
    }

    // ── Custom base URL tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_custom_base_url() {
        let mut config = test_config();
        config.base_url = "https://git.mycompany.com".to_string();

        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://git.mycompany.com/api/v4/projects/team%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1, "state": "merged"}"#,
        );

        let client = GitLabClient::with_http_client(config, mock);
        let status = client.get_pr_status("team/repo", 1).await.unwrap();
        assert_eq!(status, PrStatus::Merged);
    }

    // ── Helper method tests ───────────────────────────────────────────────

    #[test]
    fn test_review_trigger() {
        let config = test_config();
        let client = GitLabClient::new(config);
        assert_eq!(client.review_trigger(), "/claudear");
    }

    #[test]
    fn test_review_trigger_custom() {
        let mut config = test_config();
        config.review_trigger = "@mybot".to_string();
        let client = GitLabClient::new(config);
        assert_eq!(client.review_trigger(), "@mybot");
    }

    #[test]
    fn test_token_accessor() {
        let config = test_config();
        let client = GitLabClient::new(config);
        assert_eq!(client.token(), Some("test_token"));
    }

    #[test]
    fn test_token_accessor_none() {
        let config = no_token_config();
        let client = GitLabClient::new(config);
        assert_eq!(client.token(), None);
    }

    #[test]
    fn test_webhook_secret() {
        let config = test_config();
        let client = GitLabClient::new(config);
        assert_eq!(client.webhook_secret(), Some("test_secret"));
    }

    #[test]
    fn test_webhook_secret_none() {
        let mut config = test_config();
        config.webhook_secret = None;
        let client = GitLabClient::new(config);
        assert_eq!(client.webhook_secret(), None);
    }

    #[test]
    fn test_scm_provider_name() {
        let config = test_config();
        let client = GitLabClient::new(config);
        assert_eq!(ScmProvider::name(&client), "gitlab");
    }

    // ── note_to_review / note_to_comment unit tests ───────────────────────

    #[test]
    fn test_note_to_review_mapping() {
        let note = GitLabNote {
            id: 100,
            body: "Great work!".to_string(),
            author: GitLabUser {
                id: 5,
                username: "reviewer".to_string(),
            },
            created_at: "2025-01-15T10:00:00Z".to_string(),
            updated_at: "2025-01-15T11:00:00Z".to_string(),
            note_type: None,
            system: false,
            position: None,
        };

        let review = GitLabClient::<MockHttpClient>::note_to_review(&note);
        assert_eq!(review.id, 100);
        assert_eq!(review.state, "COMMENTED");
        assert_eq!(review.body.as_deref(), Some("Great work!"));
        assert_eq!(review.user.id, 5);
        assert_eq!(review.user.login, "reviewer");
        assert!(review.user.user_type.is_none());
        assert_eq!(review.submitted_at.as_deref(), Some("2025-01-15T10:00:00Z"));
        assert!(review.html_url.is_none());
    }

    #[test]
    fn test_note_to_comment_with_position() {
        let note = GitLabNote {
            id: 200,
            body: "Fix this line".to_string(),
            author: GitLabUser {
                id: 7,
                username: "code_reviewer".to_string(),
            },
            created_at: "2025-02-01T00:00:00Z".to_string(),
            updated_at: "2025-02-01T01:00:00Z".to_string(),
            note_type: Some("DiffNote".to_string()),
            system: false,
            position: Some(GitLabNotePosition {
                new_path: Some("src/lib.rs".to_string()),
                old_path: Some("src/lib.rs".to_string()),
                new_line: Some(42),
                old_line: Some(40),
            }),
        };

        let comment = GitLabClient::<MockHttpClient>::note_to_comment(&note);
        assert_eq!(comment.id, 200);
        assert_eq!(comment.body, "Fix this line");
        assert_eq!(comment.path, "src/lib.rs");
        assert_eq!(comment.line, Some(42)); // new_line preferred
        assert!(comment.start_line.is_none());
        assert_eq!(comment.user.id, 7);
        assert_eq!(comment.user.login, "code_reviewer");
        assert_eq!(comment.created_at, "2025-02-01T00:00:00Z");
        assert_eq!(comment.updated_at, "2025-02-01T01:00:00Z");
        assert!(comment.html_url.is_empty());
        assert!(comment.position.is_none());
        assert!(comment.original_position.is_none());
        assert!(comment.pull_request_review_id.is_none());
        assert!(comment.side.is_none());
    }

    #[test]
    fn test_note_to_comment_without_position() {
        let note = GitLabNote {
            id: 300,
            body: "general".to_string(),
            author: GitLabUser {
                id: 1,
                username: "user".to_string(),
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            note_type: None,
            system: false,
            position: None,
        };

        let comment = GitLabClient::<MockHttpClient>::note_to_comment(&note);
        assert!(comment.path.is_empty());
        assert!(comment.line.is_none());
        assert!(comment.start_line.is_none());
    }

    #[test]
    fn test_note_to_comment_old_line_fallback() {
        let note = GitLabNote {
            id: 400,
            body: "on deleted line".to_string(),
            author: GitLabUser {
                id: 1,
                username: "user".to_string(),
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            note_type: Some("DiffNote".to_string()),
            system: false,
            position: Some(GitLabNotePosition {
                new_path: None,
                old_path: Some("old.rs".to_string()),
                new_line: None,
                old_line: Some(99),
            }),
        };

        let comment = GitLabClient::<MockHttpClient>::note_to_comment(&note);
        assert_eq!(comment.path, "old.rs");
        assert_eq!(comment.line, Some(99));
    }

    #[test]
    fn test_note_to_comment_no_paths_at_all() {
        let note = GitLabNote {
            id: 500,
            body: "odd note".to_string(),
            author: GitLabUser {
                id: 1,
                username: "user".to_string(),
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
            updated_at: "2025-01-01T00:00:00Z".to_string(),
            note_type: Some("DiffNote".to_string()),
            system: false,
            position: Some(GitLabNotePosition {
                new_path: None,
                old_path: None,
                new_line: None,
                old_line: None,
            }),
        };

        let comment = GitLabClient::<MockHttpClient>::note_to_comment(&note);
        assert!(comment.path.is_empty());
        assert!(comment.line.is_none());
    }

    // ── MR notes pagination test ──────────────────────────────────────────

    #[tokio::test]
    async fn test_get_reviews_notes_pagination() {
        let mock = MockHttpClient::new();

        // Build a full page of 100 notes for page 1
        let mut page1_notes = String::from("[");
        for i in 0..100 {
            if i > 0 {
                page1_notes.push(',');
            }
            page1_notes.push_str(&format!(
                r#"{{
                    "id": {},
                    "body": "note {}",
                    "author": {{"id": 1, "username": "dev"}},
                    "created_at": "2025-01-01T00:00:00Z",
                    "updated_at": "2025-01-01T00:00:00Z",
                    "type": null,
                    "system": false,
                    "position": null
                }}"#,
                i + 1,
                i + 1
            ));
        }
        page1_notes.push(']');

        // Page 2 has fewer than 100 notes (pagination stops)
        let page2_notes = r#"[{
            "id": 101,
            "body": "last note",
            "author": {"id": 1, "username": "dev"},
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z",
            "type": null,
            "system": false,
            "position": null
        }]"#;

        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            &page1_notes,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=2&sort=asc",
            200,
            page2_notes,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": []}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert_eq!(
            reviews.len(),
            101,
            "should get 100 from page 1 + 1 from page 2"
        );
    }

    // ── list_repos pagination test ────────────────────────────────────────

    #[tokio::test]
    async fn test_list_repos_pagination() {
        let mock = MockHttpClient::new();

        // Build a full page of 100 projects for page 1
        let mut page1 = String::from("[");
        for i in 0..100 {
            if i > 0 {
                page1.push(',');
            }
            page1.push_str(&format!(
                r#"{{
                    "id": {},
                    "path_with_namespace": "mygroup/repo-{}",
                    "name": "repo-{}",
                    "default_branch": "main",
                    "http_url_to_repo": "https://gitlab.com/mygroup/repo-{}.git",
                    "ssh_url_to_repo": "git@gitlab.com:mygroup/repo-{}.git",
                    "web_url": "https://gitlab.com/mygroup/repo-{}",
                    "visibility": "private",
                    "archived": false
                }}"#,
                i + 1,
                i + 1,
                i + 1,
                i + 1,
                i + 1,
                i + 1
            ));
        }
        page1.push(']');

        // Page 2 has just 1 project
        let page2 = r#"[{
            "id": 101,
            "path_with_namespace": "mygroup/repo-101",
            "name": "repo-101",
            "default_branch": "main",
            "http_url_to_repo": "https://gitlab.com/mygroup/repo-101.git",
            "ssh_url_to_repo": "git@gitlab.com:mygroup/repo-101.git",
            "web_url": "https://gitlab.com/mygroup/repo-101",
            "visibility": "private",
            "archived": false
        }]"#;

        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            &page1,
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=2&include_subgroups=true",
            200,
            page2,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let repos = client.list_repos("mygroup").await.unwrap();
        assert_eq!(repos.len(), 101);
    }

    // ── MR notes API error test ───────────────────────────────────────────

    #[tokio::test]
    async fn test_get_reviews_notes_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_reviews("group/repo", 1).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_review_comments_notes_api_error() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_review_comments("group/repo", 1).await;
        assert!(result.is_err());
    }

    // ── Missing fields / invalid JSON tests ───────────────────────────────

    #[tokio::test]
    async fn test_get_pr_status_missing_state_field() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1",
            200,
            r#"{"iid": 1}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_pr_status("group/repo", 1).await;
        // state is a required field in GitLabMergeRequest, so this should fail
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_list_repos_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/projects?per_page=100&page=1&include_subgroups=true",
            200,
            "not valid json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.list_repos("mygroup").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_reviews_invalid_json_in_notes() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "definitely not json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_reviews("group/repo", 1).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_project_issues_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100",
            200,
            "{{invalid",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_project_issues("group/repo", &[], None).await;
        assert!(result.is_err());
    }

    // ── Deserialization tests for internal API types ─────────────────────

    #[test]
    fn test_deserialize_gitlab_user() {
        let json = r#"{"id": 42, "username": "jdoe"}"#;
        let user: GitLabUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.id, 42);
        assert_eq!(user.username, "jdoe");
    }

    #[test]
    fn test_deserialize_gitlab_merge_request_full() {
        let json = r#"{
            "iid": 10,
            "state": "merged",
            "source_branch": "feature/x",
            "target_branch": "main",
            "title": "Add X",
            "author": {"id": 5, "username": "dev"}
        }"#;
        let mr: GitLabMergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(mr.iid, 10);
        assert_eq!(mr.state, "merged");
        assert_eq!(mr.source_branch.as_deref(), Some("feature/x"));
        assert_eq!(mr.target_branch.as_deref(), Some("main"));
        assert_eq!(mr.title.as_deref(), Some("Add X"));
        assert!(mr.author.is_some());
        assert_eq!(mr.author.unwrap().username, "dev");
    }

    #[test]
    fn test_deserialize_gitlab_merge_request_minimal() {
        let json = r#"{"iid": 1, "state": "opened"}"#;
        let mr: GitLabMergeRequest = serde_json::from_str(json).unwrap();
        assert_eq!(mr.iid, 1);
        assert_eq!(mr.state, "opened");
        assert!(mr.source_branch.is_none());
        assert!(mr.target_branch.is_none());
        assert!(mr.title.is_none());
        assert!(mr.author.is_none());
    }

    #[test]
    fn test_deserialize_gitlab_note_full() {
        let json = r#"{
            "id": 100,
            "body": "LGTM",
            "author": {"id": 1, "username": "reviewer"},
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-02T00:00:00Z",
            "type": "DiffNote",
            "system": false,
            "position": {
                "new_path": "src/lib.rs",
                "old_path": "src/old_lib.rs",
                "new_line": 10,
                "old_line": 8
            }
        }"#;
        let note: GitLabNote = serde_json::from_str(json).unwrap();
        assert_eq!(note.id, 100);
        assert_eq!(note.body, "LGTM");
        assert_eq!(note.author.id, 1);
        assert_eq!(note.note_type.as_deref(), Some("DiffNote"));
        assert!(!note.system);
        let pos = note.position.unwrap();
        assert_eq!(pos.new_path.as_deref(), Some("src/lib.rs"));
        assert_eq!(pos.old_path.as_deref(), Some("src/old_lib.rs"));
        assert_eq!(pos.new_line, Some(10));
        assert_eq!(pos.old_line, Some(8));
    }

    #[test]
    fn test_deserialize_gitlab_note_system_no_position() {
        let json = r#"{
            "id": 200,
            "body": "changed the description",
            "author": {"id": 0, "username": "system"},
            "created_at": "2025-06-01T00:00:00Z",
            "updated_at": "2025-06-01T00:00:00Z",
            "type": null,
            "system": true,
            "position": null
        }"#;
        let note: GitLabNote = serde_json::from_str(json).unwrap();
        assert!(note.system);
        assert!(note.note_type.is_none());
        assert!(note.position.is_none());
    }

    #[test]
    fn test_deserialize_gitlab_note_position_partial() {
        let json = r#"{
            "new_path": "file.rs",
            "old_path": null,
            "new_line": null,
            "old_line": 5
        }"#;
        let pos: GitLabNotePosition = serde_json::from_str(json).unwrap();
        assert_eq!(pos.new_path.as_deref(), Some("file.rs"));
        assert!(pos.old_path.is_none());
        assert!(pos.new_line.is_none());
        assert_eq!(pos.old_line, Some(5));
    }

    #[test]
    fn test_deserialize_gitlab_approval() {
        let json = r#"{
            "user": {"id": 42, "username": "approver"},
            "created_at": "2025-03-15T12:00:00Z"
        }"#;
        let approval: GitLabApproval = serde_json::from_str(json).unwrap();
        assert_eq!(approval.user.id, 42);
        assert_eq!(approval.user.username, "approver");
        assert_eq!(approval.created_at.as_deref(), Some("2025-03-15T12:00:00Z"));
    }

    #[test]
    fn test_deserialize_gitlab_approval_no_created_at() {
        let json = r#"{
            "user": {"id": 1, "username": "user1"},
            "created_at": null
        }"#;
        let approval: GitLabApproval = serde_json::from_str(json).unwrap();
        assert!(approval.created_at.is_none());
    }

    #[test]
    fn test_deserialize_gitlab_approvals_response() {
        let json = r#"{
            "approved_by": [
                {"user": {"id": 1, "username": "a"}, "created_at": null},
                {"user": {"id": 2, "username": "b"}, "created_at": "2025-01-01T00:00:00Z"}
            ]
        }"#;
        let resp: GitLabApprovalsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.approved_by.len(), 2);
        assert_eq!(resp.approved_by[0].user.username, "a");
        assert_eq!(resp.approved_by[1].user.username, "b");
    }

    #[test]
    fn test_deserialize_gitlab_approvals_response_empty() {
        let json = r#"{"approved_by": []}"#;
        let resp: GitLabApprovalsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.approved_by.is_empty());
    }

    #[test]
    fn test_deserialize_gitlab_project_full() {
        let json = r#"{
            "id": 99,
            "path_with_namespace": "org/team/repo",
            "name": "repo",
            "default_branch": "develop",
            "http_url_to_repo": "https://gitlab.com/org/team/repo.git",
            "ssh_url_to_repo": "git@gitlab.com:org/team/repo.git",
            "web_url": "https://gitlab.com/org/team/repo",
            "visibility": "internal",
            "archived": true
        }"#;
        let project: GitLabProject = serde_json::from_str(json).unwrap();
        assert_eq!(project.id, 99);
        assert_eq!(project.path_with_namespace, "org/team/repo");
        assert_eq!(project.name, "repo");
        assert_eq!(project.default_branch.as_deref(), Some("develop"));
        assert_eq!(project.visibility.as_deref(), Some("internal"));
        assert!(project.archived);
    }

    #[test]
    fn test_deserialize_gitlab_project_null_optional_fields() {
        let json = r#"{
            "id": 1,
            "path_with_namespace": "g/r",
            "name": "r",
            "default_branch": null,
            "http_url_to_repo": "https://x.git",
            "ssh_url_to_repo": "git@x:g/r.git",
            "web_url": "https://x",
            "visibility": null,
            "archived": false
        }"#;
        let project: GitLabProject = serde_json::from_str(json).unwrap();
        assert!(project.default_branch.is_none());
        assert!(project.visibility.is_none());
        assert!(!project.archived);
    }

    #[test]
    fn test_deserialize_gitlab_diff_response() {
        let json = r#"{
            "changes": [
                {"old_path": "a.rs", "new_path": "b.rs", "diff": "@@ -1 +1 @@"},
                {"old_path": "c.rs", "new_path": "c.rs", "diff": ""}
            ]
        }"#;
        let resp: GitLabDiffResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.changes.len(), 2);
        assert_eq!(resp.changes[0].old_path, "a.rs");
        assert_eq!(resp.changes[0].new_path, "b.rs");
        assert_eq!(resp.changes[1].diff, "");
    }

    #[test]
    fn test_deserialize_gitlab_diff_response_empty() {
        let json = r#"{"changes": []}"#;
        let resp: GitLabDiffResponse = serde_json::from_str(json).unwrap();
        assert!(resp.changes.is_empty());
    }

    #[test]
    fn test_deserialize_gitlab_issue_full() {
        let json = r#"{
            "iid": 7,
            "title": "Test issue",
            "description": "A description",
            "state": "closed",
            "web_url": "https://gitlab.com/g/r/-/issues/7",
            "labels": ["bug", "urgent"],
            "author": {"id": 1, "username": "admin"},
            "assignees": [
                {"id": 2, "username": "dev1"},
                {"id": 3, "username": "dev2"}
            ]
        }"#;
        let issue: GitLabIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.iid, 7);
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.description.as_deref(), Some("A description"));
        assert_eq!(issue.state, "closed");
        assert_eq!(issue.labels, vec!["bug", "urgent"]);
        assert_eq!(issue.author.as_ref().unwrap().username, "admin");
        assert_eq!(issue.assignees.len(), 2);
    }

    #[test]
    fn test_deserialize_gitlab_issue_minimal() {
        let json = r#"{
            "iid": 1,
            "title": "Minimal",
            "description": null,
            "state": "opened",
            "web_url": "https://x",
            "labels": [],
            "author": null,
            "assignees": []
        }"#;
        let issue: GitLabIssue = serde_json::from_str(json).unwrap();
        assert!(issue.description.is_none());
        assert!(issue.author.is_none());
        assert!(issue.assignees.is_empty());
        assert!(issue.labels.is_empty());
    }

    // ── URL construction with labels containing special chars ────────────

    #[tokio::test]
    async fn test_get_project_issues_labels_with_special_chars() {
        let mock = MockHttpClient::new();
        // Labels like "priority::high" have colons which get URL-encoded
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100&labels=priority%3A%3Ahigh,auto%20fix&state=opened",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let labels = vec!["priority::high".to_string(), "auto fix".to_string()];
        let issues = client
            .get_project_issues("group/repo", &labels, Some("opened"))
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn test_get_group_issues_labels_with_special_chars() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/my%2Fgroup/issues?per_page=100&labels=auto%3A%3Aimplement&state=opened",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let labels = vec!["auto::implement".to_string()];
        let issues = client
            .get_group_issues("my/group", &labels, Some("opened"))
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    // ── Custom base URL for issue endpoints ─────────────────────────────

    #[tokio::test]
    async fn test_get_project_issues_custom_base_url() {
        let mut config = test_config();
        config.base_url = "https://git.corp.io".to_string();

        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://git.corp.io/api/v4/projects/team%2Frepo/issues?per_page=100",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(config, mock);
        let issues = client
            .get_project_issues("team/repo", &[], None)
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn test_get_group_issues_custom_base_url() {
        let mut config = test_config();
        config.base_url = "https://git.corp.io".to_string();

        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://git.corp.io/api/v4/groups/myteam/issues?per_page=100",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(config, mock);
        let issues = client.get_group_issues("myteam", &[], None).await.unwrap();
        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn test_get_issue_custom_base_url() {
        let mut config = test_config();
        config.base_url = "https://git.corp.io".to_string();

        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://git.corp.io/api/v4/projects/team%2Frepo/issues/1",
            200,
            r#"{
                "iid": 1, "title": "T", "description": null,
                "state": "opened", "web_url": "https://x",
                "labels": [], "author": null, "assignees": []
            }"#,
        );

        let client = GitLabClient::with_http_client(config, mock);
        let issue = client.get_issue("team/repo", 1).await.unwrap();
        assert_eq!(issue.iid, 1);
    }

    // ── ScmProvider trait delegation tests ───────────────────────────────

    #[test]
    fn test_scm_provider_is_enabled_delegates() {
        let config = test_config();
        let client = GitLabClient::new(config);
        // ScmProvider::is_enabled should delegate to self.is_enabled()
        assert_eq!(ScmProvider::is_enabled(&client), client.is_enabled());
    }

    #[test]
    fn test_scm_provider_review_trigger_delegates() {
        let mut config = test_config();
        config.review_trigger = "@custom-bot".to_string();
        let client = GitLabClient::new(config);
        assert_eq!(ScmProvider::review_trigger(&client), "@custom-bot");
    }

    // ── build_headers tests ─────────────────────────────────────────────

    #[test]
    fn test_build_headers_contains_expected_entries() {
        let config = test_config();
        let client =
            GitLabClient::<MockHttpClient>::with_http_client(config, MockHttpClient::new());
        let headers = client.build_headers("my_token");
        assert_eq!(headers.len(), 3);

        let token_val = headers.iter().find(|(k, _)| *k == "PRIVATE-TOKEN").unwrap();
        assert_eq!(token_val.1, "my_token");

        let accept_val = headers.iter().find(|(k, _)| *k == "Accept").unwrap();
        assert_eq!(accept_val.1, "application/json");

        let ua_val = headers.iter().find(|(k, _)| *k == "User-Agent").unwrap();
        assert_eq!(ua_val.1, "claudear");
    }

    // ── api_base tests ──────────────────────────────────────────────────

    #[test]
    fn test_api_base_returns_config_base_url() {
        let mut config = test_config();
        config.base_url = "https://custom.gitlab.io".to_string();
        let client =
            GitLabClient::<MockHttpClient>::with_http_client(config, MockHttpClient::new());
        assert_eq!(client.api_base(), "https://custom.gitlab.io");
    }

    // ── get_group_issues pagination / no labels / no state ──────────────

    #[tokio::test]
    async fn test_get_group_issues_no_labels_no_state() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/issues?per_page=100",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let issues = client.get_group_issues("mygroup", &[], None).await.unwrap();
        assert!(issues.is_empty());
    }

    // ── get_issue API error ─────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_issue_api_error_500() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues/1",
            500,
            "Internal Server Error",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_issue("group/repo", 1).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("500") || err_msg.contains("Internal Server Error"),
            "error should mention the API failure: {err_msg}"
        );
    }

    // ── with_http_client constructor test ────────────────────────────────

    #[test]
    fn test_with_http_client_constructor() {
        let config = test_config();
        let mock = MockHttpClient::new();
        let client = GitLabClient::with_http_client(config, mock);
        // Verify the client is correctly constructed
        assert!(client.is_enabled());
        assert_eq!(client.token(), Some("test_token"));
        assert_eq!(client.webhook_secret(), Some("test_secret"));
        assert_eq!(client.review_trigger(), "/claudear");
        assert_eq!(client.api_base(), "https://gitlab.com");
    }

    // ── Encode project path with unicode ────────────────────────────────

    #[test]
    fn test_encode_project_path_with_spaces() {
        assert_eq!(
            GitLabClient::<MockHttpClient>::encode_project_path("my group/my repo"),
            "my%20group%2Fmy%20repo"
        );
    }

    #[test]
    fn test_encode_project_path_with_unicode() {
        let encoded = GitLabClient::<MockHttpClient>::encode_project_path("org/repo-\u{00e9}");
        assert!(encoded.contains("%C3%A9") || encoded.contains("repo-"));
    }

    // ── get_pr_diff with multiple changes assembles correct output ──────

    #[tokio::test]
    async fn test_get_pr_diff_multiple_changes_order() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/20/changes",
            200,
            r#"{"changes": [
                {"old_path": "first.rs", "new_path": "first.rs", "diff": "diff1"},
                {"old_path": "second.rs", "new_path": "second.rs", "diff": "diff2"},
                {"old_path": "third.rs", "new_path": "third.rs", "diff": "diff3"}
            ]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let diff = client.get_pr_diff("group/repo", 20).await.unwrap();

        // Verify changes appear in order
        let first_pos = diff.find("first.rs").unwrap();
        let second_pos = diff.find("second.rs").unwrap();
        let third_pos = diff.find("third.rs").unwrap();
        assert!(first_pos < second_pos);
        assert!(second_pos < third_pos);
        assert!(diff.contains("diff1"));
        assert!(diff.contains("diff2"));
        assert!(diff.contains("diff3"));
    }

    // ── get_new_review_comments with since equal timestamp is inclusive ──

    #[tokio::test]
    async fn test_get_new_review_comments_since_equal_timestamp_included() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            r#"[{
                "id": 1,
                "body": "exact",
                "author": {"id": 1, "username": "dev"},
                "created_at": "2025-06-01T00:00:00Z",
                "updated_at": "2025-06-01T00:00:00Z",
                "type": "DiffNote",
                "system": false,
                "position": {"new_path": "f.rs", "old_path": "f.rs", "new_line": 1, "old_line": null}
            }]"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let comments = client
            .get_new_review_comments("group/repo", 1, Some("2025-06-01T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(
            comments.len(),
            1,
            "comment at exact 'since' timestamp should be included"
        );
    }

    // ── get_group_issues with only state, no labels ─────────────────────

    #[tokio::test]
    async fn test_get_group_issues_only_state_filter() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/issues?per_page=100&state=closed",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let issues = client
            .get_group_issues("mygroup", &[], Some("closed"))
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    // ── get_project_issues with only labels, no state ───────────────────

    #[tokio::test]
    async fn test_get_project_issues_only_labels_no_state() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues?per_page=100&labels=bug",
            200,
            "[]",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let labels = vec!["bug".to_string()];
        let issues = client
            .get_project_issues("group/repo", &labels, None)
            .await
            .unwrap();
        assert!(issues.is_empty());
    }

    // ── get_group_issues API error ──────────────────────────────────────

    #[tokio::test]
    async fn test_get_group_issues_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/groups/mygroup/issues?per_page=100",
            200,
            "not json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_group_issues("mygroup", &[], None).await;
        assert!(result.is_err());
    }

    // ── get_issue invalid JSON ──────────────────────────────────────────

    #[tokio::test]
    async fn test_get_issue_invalid_json() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/issues/1",
            200,
            "not json",
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let result = client.get_issue("group/repo", 1).await;
        assert!(result.is_err());
    }

    // ── get_reviews with approvals where created_at varies ──────────────

    #[tokio::test]
    async fn test_get_reviews_approval_id_is_negated_user_id() {
        let mock = MockHttpClient::new();
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/notes?per_page=100&page=1&sort=asc",
            200,
            "[]",
        );
        mock.mock_response(
            "https://gitlab.com/api/v4/projects/group%2Frepo/merge_requests/1/approvals",
            200,
            r#"{"approved_by": [
                {"user": {"id": 77, "username": "approver77"}, "created_at": "2025-01-01T00:00:00Z"}
            ]}"#,
        );

        let client = GitLabClient::with_http_client(test_config(), mock);
        let reviews = client.get_reviews("group/repo", 1).await.unwrap();
        assert_eq!(reviews.len(), 1);
        // Approval ID should be -(user.id) to avoid collision with note IDs
        assert_eq!(reviews[0].id, -77);
        assert_eq!(reviews[0].user.id, 77);
    }
}

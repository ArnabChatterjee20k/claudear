//! Jira issue source adapter.

use super::IssueSource;
use crate::config::JiraConfig;
use crate::error::{Error, Result};
use crate::http::HttpResponse;
use crate::types::{Issue, IssuePriority, IssueStatus, MatchPriority, MatchResult};
use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Deserialize;

/// Trait for HTTP client operations to enable testing.
#[async_trait]
pub trait JiraHttpClient: Send + Sync {
    /// Perform a GET request with the given auth header.
    async fn get(&self, url: &str, auth_header: &str) -> Result<HttpResponse>;

    /// Perform a POST request with the given auth header and JSON body.
    async fn post(
        &self,
        url: &str,
        auth_header: &str,
        body: serde_json::Value,
    ) -> Result<HttpResponse>;
}

/// Default HTTP client using reqwest.
pub struct ReqwestJiraClient {
    client: reqwest::Client,
}

/// Default HTTP request timeout for Jira API calls (30 seconds).
const DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;

impl ReqwestJiraClient {
    /// Create a new reqwest-based HTTP client with a default timeout.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(DEFAULT_HTTP_TIMEOUT_SECS))
                .build()
                .expect("Failed to build HTTP client"),
        }
    }
}

impl Default for ReqwestJiraClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl JiraHttpClient for ReqwestJiraClient {
    async fn get(&self, url: &str, auth_header: &str) -> Result<HttpResponse> {
        let response = self
            .client
            .get(url)
            .header("Authorization", auth_header)
            .header("Content-Type", "application/json")
            .send()
            .await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }

    async fn post(
        &self,
        url: &str,
        auth_header: &str,
        body: serde_json::Value,
    ) -> Result<HttpResponse> {
        let response = self
            .client
            .post(url)
            .header("Authorization", auth_header)
            .json(&body)
            .send()
            .await?;
        let status = response.status().as_u16();
        let body_text = response.text().await.unwrap_or_default();
        Ok(HttpResponse {
            status,
            body: body_text,
        })
    }
}

// ── Jira API response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    issues: Vec<JiraApiIssue>,
    #[allow(dead_code)]
    total: i64,
}

#[derive(Debug, Deserialize)]
struct JiraApiIssue {
    id: String,
    key: String,
    #[serde(rename = "self")]
    self_url: String,
    fields: JiraFields,
}

#[derive(Debug, Deserialize)]
struct JiraFields {
    summary: String,
    description: Option<serde_json::Value>,
    status: JiraStatus,
    priority: Option<JiraPriority>,
    issuetype: Option<JiraIssueType>,
    labels: Option<Vec<String>>,
    assignee: Option<JiraUser>,
    reporter: Option<JiraUser>,
    project: JiraProject,
    created: Option<String>,
    updated: Option<String>,
    resolution: Option<JiraResolution>,
    #[allow(dead_code)]
    comment: Option<JiraCommentContainer>,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    name: String,
    #[serde(rename = "statusCategory")]
    status_category: JiraStatusCategory,
}

#[derive(Debug, Deserialize)]
struct JiraStatusCategory {
    key: String,
    #[allow(dead_code)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraPriority {
    name: String,
    #[allow(dead_code)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraIssueType {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraUser {
    #[serde(rename = "displayName")]
    display_name: String,
    #[serde(rename = "accountId")]
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraProject {
    key: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraResolution {
    name: String,
}

#[derive(Debug, Deserialize)]
struct JiraCommentContainer {
    #[allow(dead_code)]
    comments: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionsResponse {
    transitions: Vec<JiraTransition>,
}

#[derive(Debug, Deserialize)]
struct JiraTransition {
    id: String,
    #[allow(dead_code)]
    name: String,
    to: JiraTransitionTarget,
}

#[derive(Debug, Deserialize)]
struct JiraTransitionTarget {
    #[serde(rename = "statusCategory")]
    status_category: JiraStatusCategory,
}

// ── JiraSource ───────────────────────────────────────────────────────

/// Jira REST API client.
pub struct JiraSource<H: JiraHttpClient = ReqwestJiraClient> {
    config: JiraConfig,
    http: H,
    auth_header: String,
}

impl JiraSource<ReqwestJiraClient> {
    /// Create a new Jira source with the default HTTP client.
    pub fn new(config: JiraConfig) -> Self {
        let auth_header = build_auth_header(&config);
        Self {
            config,
            http: ReqwestJiraClient::new(),
            auth_header,
        }
    }
}

impl<H: JiraHttpClient> JiraSource<H> {
    /// Create a new Jira source with a custom HTTP client.
    pub fn with_http_client(config: JiraConfig, http: H) -> Self {
        let auth_header = build_auth_header(&config);
        Self {
            config,
            http,
            auth_header,
        }
    }

    /// Build a JQL query from the configuration.
    fn build_jql(&self) -> String {
        let mut clauses = Vec::new();

        // Project filter
        if !self.config.project_keys.is_empty() {
            let projects = self
                .config
                .project_keys
                .iter()
                .map(|k| format!("\"{}\"", k))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("project in ({})", projects));
        }

        // Only unresolved issues
        clauses.push("resolution = Unresolved".to_string());

        // Status filter
        if !self.config.trigger_statuses.is_empty() {
            let statuses = self
                .config
                .trigger_statuses
                .iter()
                .map(|s| format!("\"{}\"", s))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("status in ({})", statuses));
        }

        // Label filter
        if !self.config.trigger_labels.is_empty() {
            let labels = self
                .config
                .trigger_labels
                .iter()
                .map(|l| format!("\"{}\"", l))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("labels in ({})", labels));
        }

        // Assignee filter
        if let Some(ref assignee) = self.config.trigger_assignee {
            clauses.push(format!("assignee = \"{}\"", assignee));
        }

        // Issue type filter
        if !self.config.issue_types.is_empty() {
            let types = self
                .config
                .issue_types
                .iter()
                .map(|t| format!("\"{}\"", t))
                .collect::<Vec<_>>()
                .join(", ");
            clauses.push(format!("issuetype in ({})", types));
        }

        // Custom JQL
        if let Some(ref custom) = self.config.custom_jql {
            clauses.push(format!("({})", custom));
        }

        let mut jql = clauses.join(" AND ");
        jql.push_str(" ORDER BY updated DESC");
        jql
    }

    /// Fetch issues from Jira using the constructed JQL query.
    async fn search_issues(&self) -> Result<Vec<JiraApiIssue>> {
        let jql = self.build_jql();
        let max_results = self.config.max_results.min(100);
        let fields = "summary,description,status,priority,issuetype,labels,assignee,reporter,project,created,updated,resolution,comment";

        let url = format!(
            "{}/rest/api/3/search?jql={}&maxResults={}&fields={}",
            self.config.base_url.trim_end_matches('/'),
            urlencoding::encode(&jql),
            max_results,
            fields
        );

        let response = self.http.get(&url, &self.auth_header).await?;

        if !response.is_success() {
            return Err(Error::source(
                "jira",
                format!("API error ({}): {}", response.status, response.body),
            ));
        }

        let search_response: JiraSearchResponse = response.json()?;
        Ok(search_response.issues)
    }

    /// Map a Jira API issue to the unified Issue type.
    fn map_issue(&self, api_issue: JiraApiIssue) -> Issue {
        let url = format!(
            "{}/browse/{}",
            self.config.base_url.trim_end_matches('/'),
            api_issue.key
        );

        let mut issue = Issue::new(
            &api_issue.id,
            &api_issue.key,
            &api_issue.fields.summary,
            &url,
            "jira",
        );

        // Extract description from ADF or plain text
        issue.description = api_issue.fields.description.as_ref().map(extract_adf_text);

        issue.priority = map_priority(api_issue.fields.priority.as_ref().map(|p| p.name.as_str()));
        issue.status = map_status(&api_issue.fields.status.status_category.key);

        if let Some(ref created) = api_issue.fields.created {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(created) {
                issue.created_at = Some(dt.with_timezone(&chrono::Utc));
            }
        }
        if let Some(ref updated) = api_issue.fields.updated {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(updated) {
                issue.updated_at = Some(dt.with_timezone(&chrono::Utc));
            }
        }

        // Store metadata
        issue.set_metadata("status_name", &api_issue.fields.status.name);
        issue.set_metadata(
            "status_category",
            &api_issue.fields.status.status_category.key,
        );
        issue.set_metadata("project_key", &api_issue.fields.project.key);
        issue.set_metadata("project_name", &api_issue.fields.project.name);
        issue.set_metadata("jira_id", &api_issue.id);
        issue.set_metadata("self_url", &api_issue.self_url);

        if let Some(ref priority) = api_issue.fields.priority {
            issue.set_metadata("priority_name", &priority.name);
        }

        if let Some(ref issue_type) = api_issue.fields.issuetype {
            issue.set_metadata("issue_type", &issue_type.name);
        }

        if let Some(ref labels) = api_issue.fields.labels {
            issue.set_metadata("labels", labels.join(", "));
        }

        if let Some(ref assignee) = api_issue.fields.assignee {
            issue.set_metadata("assignee", &assignee.display_name);
            if let Some(ref account_id) = assignee.account_id {
                issue.set_metadata("assignee_account_id", account_id);
            }
        }

        if let Some(ref reporter) = api_issue.fields.reporter {
            issue.set_metadata("reporter", &reporter.display_name);
        }

        if let Some(ref resolution) = api_issue.fields.resolution {
            issue.set_metadata("resolution", &resolution.name);
        }

        issue
    }
}

/// Build the Authorization header value from config.
fn build_auth_header(config: &JiraConfig) -> String {
    match config.auth_mode.as_str() {
        "bearer" => format!("Bearer {}", config.api_token),
        _ => {
            // Default to Basic auth: base64(email:token)
            let credentials = format!("{}:{}", config.email, config.api_token);
            format!("Basic {}", BASE64.encode(credentials.as_bytes()))
        }
    }
}

/// Map Jira priority name to unified IssuePriority.
fn map_priority(name: Option<&str>) -> IssuePriority {
    match name {
        Some(n) => match n.to_lowercase().as_str() {
            "highest" | "blocker" | "critical" => IssuePriority::Critical,
            "high" => IssuePriority::High,
            "medium" | "normal" => IssuePriority::Medium,
            "low" | "lowest" | "trivial" => IssuePriority::Low,
            _ => IssuePriority::None,
        },
        None => IssuePriority::None,
    }
}

/// Map Jira statusCategory key to unified IssueStatus.
fn map_status(category_key: &str) -> IssueStatus {
    match category_key {
        "new" => IssueStatus::Open,
        "indeterminate" => IssueStatus::InProgress,
        "done" => IssueStatus::Resolved,
        _ => IssueStatus::Open,
    }
}

/// Extract plain text from an Atlassian Document Format (ADF) value.
/// Handles both ADF objects (Cloud) and plain text strings (Server/DC).
fn extract_adf_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            // ADF document: recursively extract text nodes
            let mut text = String::new();
            extract_adf_text_recursive(value, &mut text);
            // Handle ADF objects that contain "content" array at top level
            if text.is_empty() {
                if let Some(content) = obj.get("content") {
                    extract_adf_text_recursive(content, &mut text);
                }
            }
            text.trim().to_string()
        }
        _ => String::new(),
    }
}

/// Recursively extract text from ADF nodes.
fn extract_adf_text_recursive(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Object(obj) => {
            let node_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");

            // Text node: extract the text content
            if node_type == "text" {
                if let Some(text) = obj.get("text").and_then(|t| t.as_str()) {
                    output.push_str(text);
                }
                return;
            }

            // Hard break
            if node_type == "hardBreak" {
                output.push('\n');
                return;
            }

            // Block-level nodes that should have newlines
            let is_block = matches!(
                node_type,
                "paragraph"
                    | "heading"
                    | "bulletList"
                    | "orderedList"
                    | "listItem"
                    | "codeBlock"
                    | "blockquote"
                    | "rule"
            );

            // Recurse into content
            if let Some(content) = obj.get("content") {
                extract_adf_text_recursive(content, output);
            }

            if is_block && !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_adf_text_recursive(item, output);
            }
        }
        _ => {}
    }
}

/// Build the metadata portion of a Jira issue context string.
fn format_jira_context(issue: &Issue) -> String {
    let mut context = format!("# Jira Issue: {}\n\n", issue.short_id);
    context.push_str(&format!("**Title:** {}\n", issue.title));
    context.push_str(&format!("**URL:** {}\n", issue.url));

    if let Some(priority_name) = issue.get_metadata::<String>("priority_name") {
        context.push_str(&format!("**Priority:** {}\n", priority_name));
    }

    if let Some(status_name) = issue.get_metadata::<String>("status_name") {
        context.push_str(&format!("**Status:** {}\n", status_name));
    }

    if let Some(issue_type) = issue.get_metadata::<String>("issue_type") {
        context.push_str(&format!("**Type:** {}\n", issue_type));
    }

    if let Some(project_name) = issue.get_metadata::<String>("project_name") {
        let project_key: Option<String> = issue.get_metadata("project_key");
        if let Some(key) = project_key {
            context.push_str(&format!("**Project:** {} ({})\n", project_name, key));
        } else {
            context.push_str(&format!("**Project:** {}\n", project_name));
        }
    }

    if let Some(assignee) = issue.get_metadata::<String>("assignee") {
        context.push_str(&format!("**Assignee:** {}\n", assignee));
    }

    if let Some(reporter) = issue.get_metadata::<String>("reporter") {
        context.push_str(&format!("**Reporter:** {}\n", reporter));
    }

    if let Some(labels) = issue.get_metadata::<String>("labels") {
        if !labels.is_empty() {
            context.push_str(&format!("**Labels:** {}\n", labels));
        }
    }

    context.push('\n');

    if let Some(ref description) = issue.description {
        if !description.is_empty() {
            context.push_str("## Description\n");
            context.push_str(description);
            context.push_str("\n\n");
        }
    }

    context
}

#[async_trait]
impl<H: JiraHttpClient + 'static> IssueSource for JiraSource<H> {
    fn name(&self) -> &str {
        "jira"
    }

    fn display_name(&self) -> &str {
        "Jira"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let api_issues = self.search_issues().await?;
        Ok(api_issues.into_iter().map(|i| self.map_issue(i)).collect())
    }

    fn matches_criteria(&self, issue: &Issue) -> MatchResult {
        // Check status category - done issues never match
        let status_category: String = issue.get_metadata("status_category").unwrap_or_default();
        if status_category == "done" {
            return MatchResult::not_matched("Issue is in a done status category");
        }

        // Check trigger_statuses match
        if !self.config.trigger_statuses.is_empty() {
            let status_name: String = issue.get_metadata("status_name").unwrap_or_default();
            let status_lower = status_name.to_lowercase();
            let matches_status = self
                .config
                .trigger_statuses
                .iter()
                .any(|s| s.to_lowercase() == status_lower);
            if !matches_status {
                return MatchResult::not_matched(format!(
                    "Status '{}' not in trigger_statuses",
                    status_name
                ));
            }
        }

        // Check trigger_assignee
        if let Some(ref trigger_assignee) = self.config.trigger_assignee {
            let assignee: Option<String> = issue.get_metadata("assignee");
            match assignee {
                Some(ref a) if a == trigger_assignee => {
                    // Assignee matches - skip label check (same pattern as Linear)
                    return MatchResult::matched(
                        format!("Assigned to {}", trigger_assignee),
                        MatchPriority::Normal,
                    );
                }
                _ => {
                    // If trigger_labels is empty, assignee mismatch means no match
                    if self.config.trigger_labels.is_empty() {
                        return MatchResult::not_matched(format!(
                            "Not assigned to {}",
                            trigger_assignee
                        ));
                    }
                    // Otherwise fall through to label check
                }
            }
        }

        // Check trigger_labels
        if !self.config.trigger_labels.is_empty() {
            let labels: String = issue.get_metadata("labels").unwrap_or_default();
            let issue_labels: Vec<&str> = if labels.is_empty() {
                vec![]
            } else {
                labels.split(", ").collect()
            };
            let has_label = self
                .config
                .trigger_labels
                .iter()
                .any(|tl| issue_labels.iter().any(|il| il == tl));
            if !has_label {
                return MatchResult::not_matched("No matching trigger labels");
            }
        }

        // Determine priority based on issue priority
        let priority = match issue.priority {
            IssuePriority::Critical => MatchPriority::Urgent,
            IssuePriority::High => MatchPriority::High,
            _ => MatchPriority::Normal,
        };

        MatchResult::matched(
            format!("Jira issue {} matches criteria", issue.short_id),
            priority,
        )
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        Ok(format_jira_context(issue))
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        let fields = "summary,description,status,priority,issuetype,labels,assignee,reporter,project,created,updated,resolution,comment";
        let url = format!(
            "{}/rest/api/3/issue/{}?fields={}",
            self.config.base_url.trim_end_matches('/'),
            issue_id,
            fields
        );

        let response = self.http.get(&url, &self.auth_header).await?;

        if !response.is_success() {
            return Err(Error::source(
                "jira",
                format!(
                    "Failed to fetch issue {} ({}): {}",
                    issue_id, response.status, response.body
                ),
            ));
        }

        let api_issue: JiraApiIssue = response.json()?;
        Ok(self.map_issue(api_issue))
    }

    async fn resolve_issue(&self, issue_id: &str) -> Result<()> {
        // First, get available transitions
        let transitions_url = format!(
            "{}/rest/api/3/issue/{}/transitions",
            self.config.base_url.trim_end_matches('/'),
            issue_id
        );

        let response = self.http.get(&transitions_url, &self.auth_header).await?;

        if !response.is_success() {
            return Err(Error::source(
                "jira",
                format!(
                    "Failed to fetch transitions for {} ({}): {}",
                    issue_id, response.status, response.body
                ),
            ));
        }

        let transitions_response: JiraTransitionsResponse = response.json()?;

        // Find a transition that moves to "done" status category
        let done_transition = transitions_response
            .transitions
            .iter()
            .find(|t| t.to.status_category.key == "done");

        let transition = match done_transition {
            Some(t) => t,
            None => {
                return Err(Error::source(
                    "jira",
                    format!(
                        "No transition to 'done' category found for issue {}",
                        issue_id
                    ),
                ));
            }
        };

        // Execute the transition
        let response = self
            .http
            .post(
                &transitions_url,
                &self.auth_header,
                serde_json::json!({
                    "transition": {
                        "id": transition.id
                    }
                }),
            )
            .await?;

        if !response.is_success() {
            return Err(Error::source(
                "jira",
                format!(
                    "Failed to resolve issue {} ({}): {}",
                    issue_id, response.status, response.body
                ),
            ));
        }

        tracing::info!(source = "jira", issue_id = %issue_id, "Resolved issue");
        Ok(())
    }

    async fn add_comment(&self, issue_id: &str, comment: &str) -> Result<()> {
        let url = format!(
            "{}/rest/api/3/issue/{}/comment",
            self.config.base_url.trim_end_matches('/'),
            issue_id
        );

        // Build comment body in ADF format
        let body = serde_json::json!({
            "body": {
                "type": "doc",
                "version": 1,
                "content": [
                    {
                        "type": "paragraph",
                        "content": [
                            {
                                "type": "text",
                                "text": comment
                            }
                        ]
                    }
                ]
            }
        });

        let response = self.http.post(&url, &self.auth_header, body).await?;

        if !response.is_success() {
            return Err(Error::source(
                "jira",
                format!(
                    "Failed to add comment to {} ({}): {}",
                    issue_id, response.status, response.body
                ),
            ));
        }

        tracing::info!(source = "jira", issue_id = %issue_id, "Added comment");
        Ok(())
    }

    async fn get_issue_status(&self, issue_id: &str) -> Result<String> {
        let issue = self.get_issue(issue_id).await?;
        let category: String = issue.get_metadata("status_category").unwrap_or_default();
        Ok(category)
    }

    fn is_terminal_status(&self, status: &str) -> bool {
        status == "done"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing.
    pub struct MockJiraClient {
        get_responses: Mutex<HashMap<String, HttpResponse>>,
        post_responses: Mutex<HashMap<String, HttpResponse>>,
        requests: Mutex<Vec<(String, String)>>,
    }

    impl MockJiraClient {
        pub fn new() -> Self {
            Self {
                get_responses: Mutex::new(HashMap::new()),
                post_responses: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub fn mock_get(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            let mut responses = self.get_responses.lock().unwrap();
            responses.insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        pub fn mock_post(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            let mut responses = self.post_responses.lock().unwrap();
            responses.insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        #[allow(dead_code)]
        pub fn get_requests(&self) -> Vec<(String, String)> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl JiraHttpClient for MockJiraClient {
        async fn get(&self, url: &str, _auth_header: &str) -> Result<HttpResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(("GET".to_string(), url.to_string()));
            let responses = self.get_responses.lock().unwrap();
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

        async fn post(
            &self,
            url: &str,
            _auth_header: &str,
            _body: serde_json::Value,
        ) -> Result<HttpResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(("POST".to_string(), url.to_string()));
            let responses = self.post_responses.lock().unwrap();
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

    fn test_config() -> JiraConfig {
        JiraConfig {
            enabled: true,
            base_url: "https://test.atlassian.net".to_string(),
            email: "user@test.com".to_string(),
            api_token: "test-token".to_string(),
            auth_mode: "basic".to_string(),
            project_keys: vec!["PROJ".to_string()],
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_statuses: vec!["To Do".to_string(), "Backlog".to_string()],
            trigger_assignee: None,
            issue_types: Vec::new(),
            custom_jql: None,
            max_results: 50,
            max_issues_per_cycle: None,
            max_concurrent: None,
            poll_interval_ms: None,
        }
    }

    fn make_jira_issue_json(
        id: &str,
        key: &str,
        summary: &str,
        status_name: &str,
        status_category_key: &str,
    ) -> String {
        serde_json::json!({
            "id": id,
            "key": key,
            "self": format!("https://test.atlassian.net/rest/api/3/issue/{}", id),
            "fields": {
                "summary": summary,
                "description": {
                    "type": "doc",
                    "version": 1,
                    "content": [
                        {
                            "type": "paragraph",
                            "content": [
                                {
                                    "type": "text",
                                    "text": "Test description"
                                }
                            ]
                        }
                    ]
                },
                "status": {
                    "name": status_name,
                    "statusCategory": {
                        "key": status_category_key,
                        "name": status_name
                    }
                },
                "priority": {
                    "name": "Medium",
                    "id": "3"
                },
                "issuetype": {
                    "name": "Bug"
                },
                "labels": ["auto-implement"],
                "assignee": {
                    "displayName": "Test User",
                    "accountId": "abc123"
                },
                "reporter": {
                    "displayName": "Reporter User",
                    "accountId": "def456"
                },
                "project": {
                    "key": "PROJ",
                    "name": "Test Project"
                },
                "created": "2024-01-01T00:00:00.000+0000",
                "updated": "2024-01-02T00:00:00.000+0000",
                "resolution": null,
                "comment": {
                    "comments": []
                }
            }
        })
        .to_string()
    }

    // ── Auth header tests ────────────────────────────────────────────

    #[test]
    fn test_basic_auth_header() {
        let config = test_config();
        let header = build_auth_header(&config);
        let expected = format!(
            "Basic {}",
            BASE64.encode("user@test.com:test-token".as_bytes())
        );
        assert_eq!(header, expected);
    }

    #[test]
    fn test_bearer_auth_header() {
        let mut config = test_config();
        config.auth_mode = "bearer".to_string();
        let header = build_auth_header(&config);
        assert_eq!(header, "Bearer test-token");
    }

    // ── JQL building tests ───────────────────────────────────────────

    #[test]
    fn test_build_jql_basic() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(jql.contains("project in (\"PROJ\")"));
        assert!(jql.contains("resolution = Unresolved"));
        assert!(jql.contains("status in (\"To Do\", \"Backlog\")"));
        assert!(jql.contains("labels in (\"auto-implement\", \"claude\")"));
        assert!(jql.contains("ORDER BY updated DESC"));
    }

    #[test]
    fn test_build_jql_with_assignee() {
        let mut config = test_config();
        config.trigger_assignee = Some("Jane Smith".to_string());
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(jql.contains("assignee = \"Jane Smith\""));
    }

    #[test]
    fn test_build_jql_with_issue_types() {
        let mut config = test_config();
        config.issue_types = vec!["Bug".to_string(), "Task".to_string()];
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(jql.contains("issuetype in (\"Bug\", \"Task\")"));
    }

    #[test]
    fn test_build_jql_with_custom_jql() {
        let mut config = test_config();
        config.custom_jql = Some("priority = High".to_string());
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(jql.contains("(priority = High)"));
    }

    #[test]
    fn test_build_jql_no_projects() {
        let mut config = test_config();
        config.project_keys = Vec::new();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(!jql.contains("project in"));
        assert!(jql.contains("resolution = Unresolved"));
    }

    #[test]
    fn test_build_jql_no_labels() {
        let mut config = test_config();
        config.trigger_labels = Vec::new();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(!jql.contains("labels in"));
    }

    #[test]
    fn test_build_jql_multiple_projects() {
        let mut config = test_config();
        config.project_keys = vec!["PROJ".to_string(), "BACKEND".to_string()];
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        let jql = source.build_jql();
        assert!(jql.contains("project in (\"PROJ\", \"BACKEND\")"));
    }

    // ── Priority mapping tests ───────────────────────────────────────

    #[test]
    fn test_priority_mapping() {
        assert_eq!(map_priority(Some("Highest")), IssuePriority::Critical);
        assert_eq!(map_priority(Some("Blocker")), IssuePriority::Critical);
        assert_eq!(map_priority(Some("Critical")), IssuePriority::Critical);
        assert_eq!(map_priority(Some("High")), IssuePriority::High);
        assert_eq!(map_priority(Some("Medium")), IssuePriority::Medium);
        assert_eq!(map_priority(Some("Normal")), IssuePriority::Medium);
        assert_eq!(map_priority(Some("Low")), IssuePriority::Low);
        assert_eq!(map_priority(Some("Lowest")), IssuePriority::Low);
        assert_eq!(map_priority(Some("Trivial")), IssuePriority::Low);
        assert_eq!(map_priority(None), IssuePriority::None);
        assert_eq!(map_priority(Some("Unknown")), IssuePriority::None);
    }

    // ── Status mapping tests ─────────────────────────────────────────

    #[test]
    fn test_status_mapping() {
        assert_eq!(map_status("new"), IssueStatus::Open);
        assert_eq!(map_status("indeterminate"), IssueStatus::InProgress);
        assert_eq!(map_status("done"), IssueStatus::Resolved);
        assert_eq!(map_status("unknown"), IssueStatus::Open);
    }

    // ── ADF extraction tests ─────────────────────────────────────────

    #[test]
    fn test_extract_adf_text_string() {
        let value = serde_json::json!("Plain text description");
        assert_eq!(extract_adf_text(&value), "Plain text description");
    }

    #[test]
    fn test_extract_adf_text_simple_doc() {
        let value = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [
                {
                    "type": "paragraph",
                    "content": [
                        {
                            "type": "text",
                            "text": "Hello world"
                        }
                    ]
                }
            ]
        });
        assert_eq!(extract_adf_text(&value), "Hello world");
    }

    #[test]
    fn test_extract_adf_text_multi_paragraph() {
        let value = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [
                {
                    "type": "paragraph",
                    "content": [
                        {"type": "text", "text": "First paragraph"}
                    ]
                },
                {
                    "type": "paragraph",
                    "content": [
                        {"type": "text", "text": "Second paragraph"}
                    ]
                }
            ]
        });
        let text = extract_adf_text(&value);
        assert!(text.contains("First paragraph"));
        assert!(text.contains("Second paragraph"));
    }

    #[test]
    fn test_extract_adf_text_with_hard_break() {
        let value = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [
                {
                    "type": "paragraph",
                    "content": [
                        {"type": "text", "text": "Line one"},
                        {"type": "hardBreak"},
                        {"type": "text", "text": "Line two"}
                    ]
                }
            ]
        });
        let text = extract_adf_text(&value);
        assert!(text.contains("Line one\nLine two"));
    }

    #[test]
    fn test_extract_adf_text_null() {
        let value = serde_json::json!(null);
        assert_eq!(extract_adf_text(&value), "");
    }

    #[test]
    fn test_extract_adf_text_empty_doc() {
        let value = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": []
        });
        assert_eq!(extract_adf_text(&value), "");
    }

    // ── matches_criteria tests ───────────────────────────────────────

    #[test]
    fn test_matches_criteria_basic_match() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("labels", "auto-implement");
        issue.priority = IssuePriority::Medium;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_done_status() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "done");
        issue.set_metadata("status_name", "Done");
        issue.set_metadata("labels", "auto-implement");

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_wrong_status() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "indeterminate");
        issue.set_metadata("status_name", "In Progress");
        issue.set_metadata("labels", "auto-implement");

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(result.reason.contains("not in trigger_statuses"));
    }

    #[test]
    fn test_matches_criteria_no_label() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("labels", "other-label");

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_assignee_match_skips_labels() {
        let mut config = test_config();
        config.trigger_assignee = Some("Test User".to_string());
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("assignee", "Test User");
        // No matching labels, but assignee matches
        issue.set_metadata("labels", "unrelated");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_assignee_mismatch_falls_to_labels() {
        let mut config = test_config();
        config.trigger_assignee = Some("Other User".to_string());
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("assignee", "Test User");
        issue.set_metadata("labels", "auto-implement");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_critical_priority() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("labels", "auto-implement");
        issue.priority = IssuePriority::Critical;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
    }

    #[test]
    fn test_matches_criteria_empty_labels_config() {
        let mut config = test_config();
        config.trigger_labels = Vec::new();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("labels", "");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_case_insensitive_status() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let mut issue = Issue::new("1", "PROJ-1", "Test", "http://test.com", "jira");
        issue.set_metadata("status_category", "new");
        issue.set_metadata("status_name", "to do"); // lowercase
        issue.set_metadata("labels", "auto-implement");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    // ── fetch / get / resolve / comment tests ────────────────────────

    #[tokio::test]
    async fn test_fetch_issues_success() {
        let config = test_config();

        let issue_json = make_jira_issue_json("10001", "PROJ-1", "Fix bug", "To Do", "new");
        let response_body = format!(r#"{{"issues": [{}], "total": 1}}"#, issue_json);

        // The URL will contain encoded JQL - mock with a prefix match approach
        // We need to mock the exact URL the source will generate
        let source = JiraSource::with_http_client(config.clone(), MockJiraClient::new());
        let jql = source.build_jql();
        let fields = "summary,description,status,priority,issuetype,labels,assignee,reporter,project,created,updated,resolution,comment";
        let expected_url = format!(
            "https://test.atlassian.net/rest/api/3/search?jql={}&maxResults=50&fields={}",
            urlencoding::encode(&jql),
            fields
        );

        let mock = MockJiraClient::new();
        mock.mock_get(&expected_url, 200, &response_body);

        let source = JiraSource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].short_id, "PROJ-1");
        assert_eq!(issues[0].title, "Fix bug");
        assert_eq!(issues[0].source, "jira");
        assert_eq!(issues[0].url, "https://test.atlassian.net/browse/PROJ-1");
    }

    #[tokio::test]
    async fn test_fetch_issues_api_error() {
        let config = test_config();
        let mock = MockJiraClient::new();
        // Don't mock any URL -> will return 404
        let source = JiraSource::with_http_client(config, mock);
        let result = source.fetch_issues().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_issue_success() {
        let config = test_config();
        let mock = MockJiraClient::new();

        let issue_json = make_jira_issue_json("10001", "PROJ-1", "Fix bug", "To Do", "new");
        let fields = "summary,description,status,priority,issuetype,labels,assignee,reporter,project,created,updated,resolution,comment";
        let url = format!(
            "https://test.atlassian.net/rest/api/3/issue/PROJ-1?fields={}",
            fields
        );
        mock.mock_get(&url, 200, &issue_json);

        let source = JiraSource::with_http_client(config, mock);
        let issue = source.get_issue("PROJ-1").await.unwrap();
        assert_eq!(issue.short_id, "PROJ-1");
        assert_eq!(issue.title, "Fix bug");
    }

    #[tokio::test]
    async fn test_get_issue_not_found() {
        let config = test_config();
        let mock = MockJiraClient::new();
        let source = JiraSource::with_http_client(config, mock);
        let result = source.get_issue("PROJ-999").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_issue_success() {
        let config = test_config();
        let mock = MockJiraClient::new();

        let transitions_url =
            "https://test.atlassian.net/rest/api/3/issue/PROJ-1/transitions".to_string();
        mock.mock_get(
            &transitions_url,
            200,
            r#"{
                "transitions": [
                    {
                        "id": "31",
                        "name": "Done",
                        "to": {
                            "statusCategory": {
                                "key": "done",
                                "name": "Done"
                            }
                        }
                    }
                ]
            }"#,
        );
        mock.mock_post(&transitions_url, 204, "");

        let source = JiraSource::with_http_client(config, mock);
        let result = source.resolve_issue("PROJ-1").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_issue_no_done_transition() {
        let config = test_config();
        let mock = MockJiraClient::new();

        let transitions_url =
            "https://test.atlassian.net/rest/api/3/issue/PROJ-1/transitions".to_string();
        mock.mock_get(
            &transitions_url,
            200,
            r#"{
                "transitions": [
                    {
                        "id": "21",
                        "name": "In Progress",
                        "to": {
                            "statusCategory": {
                                "key": "indeterminate",
                                "name": "In Progress"
                            }
                        }
                    }
                ]
            }"#,
        );

        let source = JiraSource::with_http_client(config, mock);
        let result = source.resolve_issue("PROJ-1").await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No transition to 'done'"));
    }

    #[tokio::test]
    async fn test_add_comment_success() {
        let config = test_config();
        let mock = MockJiraClient::new();

        let comment_url = "https://test.atlassian.net/rest/api/3/issue/PROJ-1/comment".to_string();
        mock.mock_post(&comment_url, 201, r#"{"id": "12345"}"#);

        let source = JiraSource::with_http_client(config, mock);
        let result = source.add_comment("PROJ-1", "Test comment").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_add_comment_failure() {
        let config = test_config();
        let mock = MockJiraClient::new();
        // No mock set -> returns 404
        let source = JiraSource::with_http_client(config, mock);
        let result = source.add_comment("PROJ-1", "Test comment").await;
        assert!(result.is_err());
    }

    // ── Context formatting tests ─────────────────────────────────────

    #[test]
    fn test_format_jira_context() {
        let mut issue = Issue::new(
            "1",
            "PROJ-1",
            "Fix the bug",
            "https://jira/browse/PROJ-1",
            "jira",
        );
        issue.description = Some("Description here".to_string());
        issue.set_metadata("priority_name", "High");
        issue.set_metadata("status_name", "To Do");
        issue.set_metadata("issue_type", "Bug");
        issue.set_metadata("project_name", "Test Project");
        issue.set_metadata("project_key", "PROJ");
        issue.set_metadata("assignee", "Jane Smith");
        issue.set_metadata("reporter", "John Doe");
        issue.set_metadata("labels", "auto-implement, bug");

        let context = format_jira_context(&issue);
        assert!(context.contains("# Jira Issue: PROJ-1"));
        assert!(context.contains("**Title:** Fix the bug"));
        assert!(context.contains("**Priority:** High"));
        assert!(context.contains("**Status:** To Do"));
        assert!(context.contains("**Type:** Bug"));
        assert!(context.contains("**Project:** Test Project (PROJ)"));
        assert!(context.contains("**Assignee:** Jane Smith"));
        assert!(context.contains("**Reporter:** John Doe"));
        assert!(context.contains("**Labels:** auto-implement, bug"));
        assert!(context.contains("## Description"));
        assert!(context.contains("Description here"));
    }

    #[test]
    fn test_format_jira_context_minimal() {
        let issue = Issue::new(
            "1",
            "PROJ-1",
            "Fix the bug",
            "https://jira/browse/PROJ-1",
            "jira",
        );
        let context = format_jira_context(&issue);
        assert!(context.contains("# Jira Issue: PROJ-1"));
        assert!(context.contains("**Title:** Fix the bug"));
        assert!(!context.contains("## Description"));
    }

    // ── Issue mapping tests ──────────────────────────────────────────

    #[test]
    fn test_map_issue_basic() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let json = make_jira_issue_json("10001", "PROJ-1", "Fix bug", "To Do", "new");
        let api_issue: JiraApiIssue = serde_json::from_str(&json).unwrap();
        let issue = source.map_issue(api_issue);

        assert_eq!(issue.id, "10001");
        assert_eq!(issue.short_id, "PROJ-1");
        assert_eq!(issue.title, "Fix bug");
        assert_eq!(issue.source, "jira");
        assert_eq!(issue.url, "https://test.atlassian.net/browse/PROJ-1");
        assert_eq!(issue.priority, IssuePriority::Medium);
        assert_eq!(issue.status, IssueStatus::Open);

        assert_eq!(
            issue.get_metadata::<String>("status_name"),
            Some("To Do".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("status_category"),
            Some("new".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("project_key"),
            Some("PROJ".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("assignee"),
            Some("Test User".to_string())
        );
        assert_eq!(
            issue.get_metadata::<String>("issue_type"),
            Some("Bug".to_string())
        );
    }

    #[test]
    fn test_map_issue_description_extraction() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());

        let json = make_jira_issue_json("10001", "PROJ-1", "Fix bug", "To Do", "new");
        let api_issue: JiraApiIssue = serde_json::from_str(&json).unwrap();
        let issue = source.map_issue(api_issue);

        assert_eq!(issue.description, Some("Test description".to_string()));
    }

    // ── Terminal status tests ────────────────────────────────────────

    #[test]
    fn test_is_terminal_status() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        assert!(source.is_terminal_status("done"));
        assert!(!source.is_terminal_status("new"));
        assert!(!source.is_terminal_status("indeterminate"));
    }

    // ── Source name tests ────────────────────────────────────────────

    #[test]
    fn test_source_name_display() {
        let config = test_config();
        let source = JiraSource::with_http_client(config, MockJiraClient::new());
        assert_eq!(source.name(), "jira");
        assert_eq!(source.display_name(), "Jira");
    }

    #[tokio::test]
    async fn test_get_issue_status() {
        let config = test_config();
        let mock = MockJiraClient::new();

        let issue_json = make_jira_issue_json("10001", "PROJ-1", "Fix bug", "To Do", "new");
        let fields = "summary,description,status,priority,issuetype,labels,assignee,reporter,project,created,updated,resolution,comment";
        let url = format!(
            "https://test.atlassian.net/rest/api/3/issue/PROJ-1?fields={}",
            fields
        );
        mock.mock_get(&url, 200, &issue_json);

        let source = JiraSource::with_http_client(config, mock);
        let status = source.get_issue_status("PROJ-1").await.unwrap();
        assert_eq!(status, "new");
    }
}

//! Linear issue source adapter.

use super::IssueSource;
use crate::config::LinearConfig;
use crate::error::{Error, Result};
use crate::types::{Issue, IssuePriority, IssueStatus, MatchPriority, MatchResult};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// HTTP response abstraction for testability.
#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    /// Check if the status is successful (2xx).
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Parse the body as JSON.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.body)
            .map_err(|e| Error::Other(format!("JSON parse error: {}", e)))
    }
}

/// Trait for GraphQL client operations to enable testing.
#[async_trait]
pub trait LinearHttpClient: Send + Sync {
    /// Perform a POST request with JSON body.
    async fn post(&self, url: &str, api_key: &str, body: serde_json::Value)
        -> Result<HttpResponse>;
}

/// Default HTTP client using reqwest.
pub struct ReqwestLinearClient {
    client: reqwest::Client,
}

impl ReqwestLinearClient {
    /// Create a new reqwest-based HTTP client.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestLinearClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl LinearHttpClient for ReqwestLinearClient {
    async fn post(
        &self,
        url: &str,
        api_key: &str,
        body: serde_json::Value,
    ) -> Result<HttpResponse> {
        let response = self
            .client
            .post(url)
            .header("Authorization", api_key)
            .header("Content-Type", "application/json")
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

/// Linear GraphQL API client.
pub struct LinearSource<H: LinearHttpClient = ReqwestLinearClient> {
    config: LinearConfig,
    http: H,
}

// GraphQL types
#[derive(Debug, Serialize)]
struct GraphQLRequest {
    query: &'static str,
    variables: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GraphQLResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphQLError>>,
}

#[derive(Debug, Deserialize)]
struct GraphQLError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct IssuesResponse {
    issues: IssuesConnection,
}

#[derive(Debug, Deserialize)]
struct IssuesConnection {
    nodes: Vec<LinearIssue>,
}

#[derive(Debug, Deserialize)]
struct IssueResponse {
    issue: Option<LinearIssue>,
}

#[derive(Debug, Deserialize)]
struct LinearIssue {
    id: String,
    identifier: String,
    title: String,
    description: Option<String>,
    url: String,
    priority: i32,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    state: Option<LinearState>,
    labels: LabelsConnection,
    team: Option<LinearTeam>,
    project: Option<LinearProject>,
    assignee: Option<LinearUser>,
}

#[derive(Debug, Deserialize)]
struct LinearState {
    name: String,
    #[serde(rename = "type")]
    state_type: String,
}

#[derive(Debug, Deserialize)]
struct LabelsConnection {
    nodes: Vec<LinearLabel>,
}

#[derive(Debug, Deserialize)]
struct LinearLabel {
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearTeam {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearProject {
    id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct LinearUser {
    name: String,
}

const ISSUES_QUERY: &str = r#"
query Issues($filter: IssueFilter, $first: Int) {
  issues(filter: $filter, first: $first, orderBy: updatedAt) {
    nodes {
      id
      identifier
      title
      description
      url
      priority
      createdAt
      updatedAt
      state {
        name
        type
      }
      labels {
        nodes {
          name
        }
      }
      team {
        id
        name
      }
      project {
        id
        name
      }
      assignee {
        name
      }
    }
  }
}
"#;

const ISSUE_QUERY: &str = r#"
query Issue($id: String!) {
  issue(id: $id) {
    id
    identifier
    title
    description
    url
    priority
    createdAt
    updatedAt
    state {
      name
      type
    }
    labels {
      nodes {
        name
      }
    }
    team {
      id
      name
    }
    project {
      id
      name
    }
    assignee {
      name
    }
  }
}
"#;

impl LinearSource<ReqwestLinearClient> {
    /// Create a new Linear source with the default HTTP client.
    pub fn new(config: LinearConfig) -> Self {
        Self {
            config,
            http: ReqwestLinearClient::new(),
        }
    }
}

impl<H: LinearHttpClient> LinearSource<H> {
    /// Create a new Linear source with a custom HTTP client.
    pub fn with_http_client(config: LinearConfig, http: H) -> Self {
        Self { config, http }
    }

    async fn graphql<T: for<'de> Deserialize<'de>>(
        &self,
        query: &'static str,
        variables: serde_json::Value,
    ) -> Result<T> {
        let request = GraphQLRequest { query, variables };
        let body = serde_json::to_value(&request)
            .map_err(|e| Error::Other(format!("JSON error: {}", e)))?;

        let response = self
            .http
            .post("https://api.linear.app/graphql", &self.config.api_key, body)
            .await?;

        if !response.is_success() {
            return Err(Error::source(
                "linear",
                format!("API error: {}", response.body),
            ));
        }

        let gql_response: GraphQLResponse<T> = response.json()?;

        if let Some(errors) = gql_response.errors {
            let messages: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            return Err(Error::source("linear", messages.join(", ")));
        }

        gql_response
            .data
            .ok_or_else(|| Error::source("linear", "No data in response"))
    }

    fn map_issue(&self, issue: LinearIssue) -> Issue {
        let labels: Vec<String> = issue.labels.nodes.iter().map(|l| l.name.clone()).collect();

        let mut mapped = Issue::new(issue.id, issue.identifier, issue.title, issue.url, "linear");

        mapped.description = issue.description;
        mapped.priority = Self::map_priority(issue.priority);
        mapped.status = Self::map_status(issue.state.as_ref().map(|s| s.state_type.as_str()));

        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&issue.created_at) {
            mapped.created_at = Some(dt.with_timezone(&chrono::Utc));
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&issue.updated_at) {
            mapped.updated_at = Some(dt.with_timezone(&chrono::Utc));
        }

        // Store metadata
        if let Some(ref state) = issue.state {
            mapped.set_metadata("state_name", &state.name);
            mapped.set_metadata("state_type", &state.state_type);
        }
        mapped.set_metadata("labels", &labels);
        if let Some(ref team) = issue.team {
            mapped.set_metadata("team", &team.name);
            mapped.set_metadata("team_id", &team.id);
        }
        if let Some(ref project) = issue.project {
            mapped.set_metadata("project", &project.name);
            mapped.set_metadata("project_id", &project.id);
        }
        if let Some(ref assignee) = issue.assignee {
            mapped.set_metadata("assignee", &assignee.name);
        }

        mapped
    }

    fn map_priority(priority: i32) -> IssuePriority {
        match priority {
            1 => IssuePriority::Critical,
            2 => IssuePriority::High,
            3 => IssuePriority::Medium,
            4 => IssuePriority::Low,
            _ => IssuePriority::None,
        }
    }

    fn map_status(state_type: Option<&str>) -> IssueStatus {
        match state_type {
            Some("completed") | Some("canceled") => IssueStatus::Resolved,
            Some("started") => IssueStatus::InProgress,
            _ => IssueStatus::Open,
        }
    }

    /// Check if a Linear state type represents a terminal state.
    /// Terminal states are those where the issue is considered "done" - no further action needed.
    pub fn is_issue_terminal(state_type: &str) -> bool {
        let s = state_type.to_lowercase();
        s == "completed" || s == "canceled" || s == "cancelled"
    }
}

#[async_trait]
impl<H: LinearHttpClient + 'static> IssueSource for LinearSource<H> {
    fn name(&self) -> &str {
        "linear"
    }

    fn display_name(&self) -> &str {
        "Linear"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        let mut filter = serde_json::Map::new();

        if let Some(ref team_id) = self.config.team_id {
            if !team_id.is_empty() {
                filter.insert(
                    "team".to_string(),
                    serde_json::json!({ "id": { "eq": team_id } }),
                );
            }
        }

        if let Some(ref project_id) = self.config.project_id {
            if !project_id.is_empty() {
                filter.insert(
                    "project".to_string(),
                    serde_json::json!({ "id": { "eq": project_id } }),
                );
            }
        }

        if !self.config.trigger_labels.is_empty() {
            filter.insert(
                "labels".to_string(),
                serde_json::json!({
                    "some": { "name": { "in": self.config.trigger_labels } }
                }),
            );
        }

        let variables = serde_json::json!({
            "filter": filter,
            "first": 50
        });

        let response: IssuesResponse = self.graphql(ISSUES_QUERY, variables).await?;

        Ok(response
            .issues
            .nodes
            .into_iter()
            .map(|i| self.map_issue(i))
            .collect())
    }

    fn matches_criteria(&self, issue: &Issue) -> MatchResult {
        let state_name: Option<String> = issue.get_metadata("state_name");
        let state_type: Option<String> = issue.get_metadata("state_type");
        let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();

        // Check state
        if !self.config.trigger_states.is_empty() {
            let state_name_lower = state_name.as_deref().unwrap_or("").to_lowercase();
            let state_type_lower = state_type.as_deref().unwrap_or("").to_lowercase();

            let state_matches = self.config.trigger_states.iter().any(|s| {
                let s_lower = s.to_lowercase();
                state_name_lower.contains(&s_lower) || state_type_lower.contains(&s_lower)
            });

            if !state_matches {
                return MatchResult::not_matched(format!(
                    "State \"{}\" not in trigger states",
                    state_name.as_deref().unwrap_or("unknown")
                ));
            }
        }

        // Check labels
        if !self.config.trigger_labels.is_empty() {
            let label_matches = self.config.trigger_labels.iter().any(|trigger| {
                labels
                    .iter()
                    .any(|l| l.to_lowercase() == trigger.to_lowercase())
            });

            if !label_matches {
                return MatchResult::not_matched("No matching trigger labels");
            }
        }

        // Determine priority
        let priority = match issue.priority {
            IssuePriority::Critical => MatchPriority::Urgent,
            IssuePriority::High => MatchPriority::High,
            IssuePriority::Low => MatchPriority::Low,
            _ => MatchPriority::Normal,
        };

        MatchResult::matched("Matches state and label criteria", priority)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("# Linear Issue: {}\n\n", issue.short_id);
        context.push_str(&format!("**Title:** {}\n", issue.title));
        context.push_str(&format!("**URL:** {}\n", issue.url));
        context.push_str(&format!("**Priority:** {}\n", issue.priority));
        context.push_str(&format!("**Status:** {}\n\n", issue.status));

        if let Some(ref description) = issue.description {
            context.push_str(&format!("## Description\n{}\n\n", description));
        }

        if let Some(team) = issue.get_metadata::<String>("team") {
            context.push_str(&format!("**Team:** {}\n", team));
        }
        if let Some(project) = issue.get_metadata::<String>("project") {
            context.push_str(&format!("**Project:** {}\n", project));
        }
        if let Some(assignee) = issue.get_metadata::<String>("assignee") {
            context.push_str(&format!("**Assignee:** {}\n", assignee));
        }

        Ok(context)
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        let variables = serde_json::json!({
            "id": issue_id
        });

        let response: IssueResponse = self.graphql(ISSUE_QUERY, variables).await?;

        response
            .issue
            .map(|i| self.map_issue(i))
            .ok_or_else(|| Error::issue_not_found("linear", issue_id))
    }

    async fn resolve_issue(&self, issue_id: &str) -> Result<()> {
        // First, get the "Done" state ID for this issue's team
        let issue = self.get_issue(issue_id).await?;
        let team_id = issue
            .get_metadata::<String>("team_id")
            .ok_or_else(|| Error::source("linear", "Issue has no team_id"))?;

        // Query for team's workflow states to find "Done" state
        #[derive(Debug, Deserialize)]
        struct TeamStatesResponse {
            team: Option<TeamWithStates>,
        }
        #[derive(Debug, Deserialize)]
        struct TeamWithStates {
            states: StatesConnection,
        }
        #[derive(Debug, Deserialize)]
        struct StatesConnection {
            nodes: Vec<WorkflowState>,
        }
        #[derive(Debug, Deserialize)]
        struct WorkflowState {
            id: String,
            name: String,
            #[serde(rename = "type")]
            state_type: String,
        }

        const TEAM_STATES_QUERY: &str = r#"
            query TeamStates($teamId: String!) {
                team(id: $teamId) {
                    states {
                        nodes {
                            id
                            name
                            type
                        }
                    }
                }
            }
        "#;

        let states_response: TeamStatesResponse = self
            .graphql(TEAM_STATES_QUERY, serde_json::json!({ "teamId": team_id }))
            .await?;

        let done_state = states_response
            .team
            .and_then(|t| {
                t.states
                    .nodes
                    .into_iter()
                    .find(|s| s.state_type == "completed")
            })
            .ok_or_else(|| Error::source("linear", "Could not find completed state for team"))?;

        // Update the issue state to Done
        #[derive(Debug, Deserialize)]
        struct IssueUpdateResponse {
            #[serde(rename = "issueUpdate")]
            issue_update: Option<IssueUpdatePayload>,
        }
        #[derive(Debug, Deserialize)]
        struct IssueUpdatePayload {
            success: bool,
        }

        const ISSUE_UPDATE_MUTATION: &str = r#"
            mutation UpdateIssue($id: String!, $stateId: String!) {
                issueUpdate(id: $id, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;

        let update_response: IssueUpdateResponse = self
            .graphql(
                ISSUE_UPDATE_MUTATION,
                serde_json::json!({
                    "id": issue_id,
                    "stateId": done_state.id
                }),
            )
            .await?;

        if !update_response
            .issue_update
            .map(|u| u.success)
            .unwrap_or(false)
        {
            return Err(Error::source("linear", "Failed to update issue state"));
        }

        tracing::info!(
            source = "linear",
            issue_id = %issue_id,
            state = %done_state.name,
            "Resolved issue"
        );
        Ok(())
    }

    async fn add_comment(&self, issue_id: &str, comment: &str) -> Result<()> {
        #[derive(Debug, Deserialize)]
        struct CommentCreateResponse {
            #[serde(rename = "commentCreate")]
            comment_create: Option<CommentCreatePayload>,
        }
        #[derive(Debug, Deserialize)]
        struct CommentCreatePayload {
            success: bool,
        }

        const COMMENT_CREATE_MUTATION: &str = r#"
            mutation CreateComment($issueId: String!, $body: String!) {
                commentCreate(input: { issueId: $issueId, body: $body }) {
                    success
                }
            }
        "#;

        let response: CommentCreateResponse = self
            .graphql(
                COMMENT_CREATE_MUTATION,
                serde_json::json!({
                    "issueId": issue_id,
                    "body": comment
                }),
            )
            .await?;

        if !response.comment_create.map(|c| c.success).unwrap_or(false) {
            return Err(Error::source("linear", "Failed to create comment"));
        }

        tracing::info!(source = "linear", issue_id = %issue_id, "Added comment to issue");
        Ok(())
    }

    async fn get_issue_status(&self, issue_id: &str) -> Result<String> {
        let issue = self.get_issue(issue_id).await?;
        let state_type: Option<String> = issue.get_metadata("state_type");
        Ok(state_type.unwrap_or_else(|| "unknown".to_string()))
    }

    fn is_terminal_status(&self, status: &str) -> bool {
        Self::is_issue_terminal(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing.
    pub struct MockLinearClient {
        responses: Mutex<HashMap<String, HttpResponse>>,
        requests: Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl MockLinearClient {
        pub fn new() -> Self {
            Self {
                responses: Mutex::new(HashMap::new()),
                requests: Mutex::new(Vec::new()),
            }
        }

        pub fn mock_response(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            let mut responses = self.responses.lock().unwrap();
            responses.insert(
                url.into(),
                HttpResponse {
                    status,
                    body: body.into(),
                },
            );
        }

        #[allow(dead_code)]
        pub fn get_requests(&self) -> Vec<(String, serde_json::Value)> {
            self.requests.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LinearHttpClient for MockLinearClient {
        async fn post(
            &self,
            url: &str,
            _api_key: &str,
            body: serde_json::Value,
        ) -> Result<HttpResponse> {
            self.requests.lock().unwrap().push((url.to_string(), body));
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

    #[test]
    fn test_http_response_is_success() {
        let response = HttpResponse {
            status: 200,
            body: "{}".to_string(),
        };
        assert!(response.is_success());
        let response = HttpResponse {
            status: 404,
            body: "{}".to_string(),
        };
        assert!(!response.is_success());
    }

    #[tokio::test]
    async fn test_fetch_issues_success() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "data": {
                    "issues": {
                        "nodes": [{
                            "id": "123",
                            "identifier": "PROJ-123",
                            "title": "Test Issue",
                            "description": "Description",
                            "url": "https://linear.app/123",
                            "priority": 2,
                            "createdAt": "2024-01-01T00:00:00Z",
                            "updatedAt": "2024-01-02T00:00:00Z",
                            "state": {"name": "Backlog", "type": "backlog"},
                            "labels": {"nodes": [{"name": "auto-implement"}]},
                            "team": {"id": "team1", "name": "Team"},
                            "project": null,
                            "assignee": null
                        }]
                    }
                }
            }"#,
        );

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].short_id, "PROJ-123");
        assert_eq!(issues[0].title, "Test Issue");
    }

    #[tokio::test]
    async fn test_fetch_issues_api_error() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            500,
            "Internal Server Error",
        );

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let result = source.fetch_issues().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_fetch_issues_graphql_error() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "errors": [{"message": "Invalid query"}]
            }"#,
        );

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let result = source.fetch_issues().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid query"));
    }

    #[tokio::test]
    async fn test_fetch_issues_no_data() {
        let mock = MockLinearClient::new();
        mock.mock_response("https://api.linear.app/graphql", 200, r#"{}"#);

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let result = source.fetch_issues().await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No data"));
    }

    #[tokio::test]
    async fn test_get_issue_success() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "data": {
                    "issue": {
                        "id": "456",
                        "identifier": "PROJ-456",
                        "title": "Single Issue",
                        "description": "Desc",
                        "url": "https://linear.app/456",
                        "priority": 1,
                        "createdAt": "2024-01-01T00:00:00Z",
                        "updatedAt": "2024-01-02T00:00:00Z",
                        "state": {"name": "In Progress", "type": "started"},
                        "labels": {"nodes": []},
                        "team": null,
                        "project": null,
                        "assignee": {"name": "John Doe"}
                    }
                }
            }"#,
        );

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let issue = source.get_issue("456").await.unwrap();

        assert_eq!(issue.id, "456");
        assert_eq!(issue.short_id, "PROJ-456");
        assert_eq!(issue.status, IssueStatus::InProgress);
    }

    #[tokio::test]
    async fn test_get_issue_not_found() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "data": {
                    "issue": null
                }
            }"#,
        );

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);
        let result = source.get_issue("nonexistent").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_build_issue_context() {
        let mock = MockLinearClient::new();
        // No HTTP call needed for build_issue_context - it just formats metadata

        let config = test_config();
        let source = LinearSource::with_http_client(config, mock);

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Test Issue",
            "https://linear.app/123",
            "linear",
        );
        issue.description = Some("This is the description".to_string());
        issue.set_metadata("team", "Engineering");
        issue.set_metadata("project", "Project Alpha");
        issue.set_metadata("assignee", "John");

        let context = source.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("PROJ-123"));
        assert!(context.contains("Test Issue"));
        assert!(context.contains("This is the description"));
        assert!(context.contains("Engineering"));
        assert!(context.contains("Project Alpha"));
        assert!(context.contains("John"));
    }

    #[tokio::test]
    async fn test_fetch_issues_with_team_filter() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "data": {
                    "issues": {
                        "nodes": []
                    }
                }
            }"#,
        );

        let mut config = test_config();
        config.team_id = Some("team-123".to_string());
        let source = LinearSource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert!(issues.is_empty());
    }

    #[tokio::test]
    async fn test_fetch_issues_with_project_filter() {
        let mock = MockLinearClient::new();
        mock.mock_response(
            "https://api.linear.app/graphql",
            200,
            r#"{
                "data": {
                    "issues": {
                        "nodes": []
                    }
                }
            }"#,
        );

        let mut config = test_config();
        config.project_id = Some("proj-123".to_string());
        let source = LinearSource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert!(issues.is_empty());
    }

    fn test_config() -> LinearConfig {
        LinearConfig {
            enabled: true,
            api_key: "test_key".to_string(),
            trigger_labels: vec!["auto-implement".to_string(), "claude".to_string()],
            trigger_states: vec!["backlog".to_string(), "todo".to_string()],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_map_priority() {
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(1),
            IssuePriority::Critical
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(2),
            IssuePriority::High
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(3),
            IssuePriority::Medium
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(4),
            IssuePriority::Low
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(0),
            IssuePriority::None
        );
    }

    #[test]
    fn test_map_status() {
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("completed")),
            IssueStatus::Resolved
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("canceled")),
            IssueStatus::Resolved
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("started")),
            IssueStatus::InProgress
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("backlog")),
            IssueStatus::Open
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(None),
            IssueStatus::Open
        );
    }

    #[test]
    fn test_matches_criteria_labels() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["auto-implement".to_string()]);
        issue.set_metadata("state_name", "Backlog");
        issue.set_metadata("state_type", "backlog");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);

        // No matching labels
        issue.set_metadata("labels", vec!["other-label".to_string()]);
        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_states() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_name", "Todo");
        issue.set_metadata("state_type", "todo");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);

        // Non-matching state
        issue.set_metadata("state_name", "In Progress");
        issue.set_metadata("state_type", "started");
        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_priority_mapping() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_type", "backlog");
        issue.priority = IssuePriority::Critical;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);

        issue.priority = IssuePriority::High;
        let result = source.matches_criteria(&issue);
        assert_eq!(result.priority, MatchPriority::High);

        issue.priority = IssuePriority::Low;
        let result = source.matches_criteria(&issue);
        assert_eq!(result.priority, MatchPriority::Low);
    }

    #[test]
    fn test_source_name() {
        let source = LinearSource::new(test_config());
        assert_eq!(source.name(), "linear");
        assert_eq!(source.display_name(), "Linear");
    }

    #[test]
    fn test_matches_criteria_empty_trigger_labels() {
        let config = LinearConfig {
            enabled: true,
            api_key: "test_key".to_string(),
            trigger_labels: vec![], // Empty - matches all
            trigger_states: vec!["backlog".to_string()],
            team_id: None,
            project_id: None,
            webhook_secret: None,
            ..Default::default()
        };
        let source = LinearSource::new(config);

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["any-label".to_string()]);
        issue.set_metadata("state_type", "backlog");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_empty_trigger_states() {
        let config = LinearConfig {
            enabled: true,
            api_key: "test_key".to_string(),
            trigger_labels: vec!["auto-implement".to_string()],
            trigger_states: vec![], // Empty - matches all
            team_id: None,
            project_id: None,
            webhook_secret: None,
            ..Default::default()
        };
        let source = LinearSource::new(config);

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["auto-implement".to_string()]);
        issue.set_metadata("state_type", "any-state");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_case_insensitive_labels() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["AUTO-IMPLEMENT".to_string()]); // Uppercase
        issue.set_metadata("state_type", "backlog");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_case_insensitive_states() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_name", "BACKLOG"); // Uppercase
        issue.set_metadata("state_type", "BACKLOG");

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_matches_criteria_priority_medium() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_type", "backlog");
        issue.priority = IssuePriority::Medium;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
    }

    #[test]
    fn test_matches_criteria_priority_none() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_type", "backlog");
        issue.priority = IssuePriority::None;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
    }

    #[test]
    fn test_matches_criteria_no_metadata() {
        let source = LinearSource::new(test_config());

        // Issue with no metadata set
        let issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );

        let result = source.matches_criteria(&issue);
        // Should not match because no labels or state
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_partial_state_match() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_name", "In Backlog"); // Contains "backlog"
        issue.set_metadata("state_type", "unstarted");

        let result = source.matches_criteria(&issue);
        // Should match because state_name contains "backlog"
        assert!(result.matches);
    }

    #[test]
    fn test_map_priority_all_values() {
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(0),
            IssuePriority::None
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(1),
            IssuePriority::Critical
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(2),
            IssuePriority::High
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(3),
            IssuePriority::Medium
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(4),
            IssuePriority::Low
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(5),
            IssuePriority::None
        ); // Out of range
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(-1),
            IssuePriority::None
        ); // Negative
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_priority(100),
            IssuePriority::None
        );
    }

    #[test]
    fn test_map_status_all_values() {
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("completed")),
            IssueStatus::Resolved
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("canceled")),
            IssueStatus::Resolved
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("started")),
            IssueStatus::InProgress
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("backlog")),
            IssueStatus::Open
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("triage")),
            IssueStatus::Open
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("unstarted")),
            IssueStatus::Open
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(Some("")),
            IssueStatus::Open
        );
        assert_eq!(
            LinearSource::<ReqwestLinearClient>::map_status(None),
            IssueStatus::Open
        );
    }

    #[test]
    fn test_config_with_filters() {
        let config = LinearConfig {
            enabled: true,
            api_key: "test_key".to_string(),
            trigger_labels: vec!["urgent".to_string()],
            trigger_states: vec!["todo".to_string()],
            team_id: Some("team_123".to_string()),
            project_id: Some("project_456".to_string()),
            webhook_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let source = LinearSource::new(config);

        // Verify source was created (API calls would require mocking)
        assert_eq!(source.name(), "linear");
    }

    #[test]
    fn test_matches_criteria_reason_message() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["claude".to_string()]);
        issue.set_metadata("state_name", "In Progress");
        issue.set_metadata("state_type", "started"); // Not in trigger_states

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(!result.reason.is_empty());
        assert!(result.reason.contains("In Progress"));
    }

    #[test]
    fn test_matches_criteria_no_matching_labels_message() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-123",
            "Fix bug",
            "https://example.com",
            "linear",
        );
        issue.set_metadata("labels", vec!["unrelated".to_string()]);
        issue.set_metadata("state_type", "backlog");

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(!result.reason.is_empty());
        assert!(result.reason.contains("label"));
    }

    fn create_linear_issue(
        id: &str,
        identifier: &str,
        title: &str,
        priority: i32,
        state_type: &str,
        state_name: &str,
        labels: Vec<&str>,
    ) -> LinearIssue {
        LinearIssue {
            id: id.to_string(),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some("Test description".to_string()),
            url: format!("https://linear.app/team/issue/{}", identifier),
            priority,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-02T00:00:00Z".to_string(),
            state: Some(LinearState {
                name: state_name.to_string(),
                state_type: state_type.to_string(),
            }),
            labels: LabelsConnection {
                nodes: labels
                    .iter()
                    .map(|l| LinearLabel {
                        name: l.to_string(),
                    })
                    .collect(),
            },
            team: Some(LinearTeam {
                id: "team_123".to_string(),
                name: "Engineering".to_string(),
            }),
            project: Some(LinearProject {
                id: "proj_456".to_string(),
                name: "Backend".to_string(),
            }),
            assignee: Some(LinearUser {
                name: "John Doe".to_string(),
            }),
        }
    }

    #[test]
    fn test_map_issue_full() {
        let source = LinearSource::new(test_config());
        let linear_issue = create_linear_issue(
            "issue_123",
            "PROJ-123",
            "Fix authentication bug",
            1, // Critical
            "started",
            "In Progress",
            vec!["bug", "auth"],
        );

        let issue = source.map_issue(linear_issue);

        assert_eq!(issue.id, "issue_123");
        assert_eq!(issue.short_id, "PROJ-123");
        assert_eq!(issue.title, "Fix authentication bug");
        assert_eq!(issue.source, "linear");
        assert_eq!(issue.priority, IssuePriority::Critical);
        assert_eq!(issue.status, IssueStatus::InProgress);
        assert_eq!(issue.description, Some("Test description".to_string()));
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());

        // Check metadata
        let team: Option<String> = issue.get_metadata("team");
        assert_eq!(team, Some("Engineering".to_string()));

        let project: Option<String> = issue.get_metadata("project");
        assert_eq!(project, Some("Backend".to_string()));

        let assignee: Option<String> = issue.get_metadata("assignee");
        assert_eq!(assignee, Some("John Doe".to_string()));

        let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"bug".to_string()));
    }

    #[test]
    fn test_map_issue_minimal() {
        let source = LinearSource::new(test_config());
        let linear_issue = LinearIssue {
            id: "123".to_string(),
            identifier: "TEST-1".to_string(),
            title: "Simple issue".to_string(),
            description: None,
            url: "https://linear.app/test/TEST-1".to_string(),
            priority: 0,
            created_at: "invalid-date".to_string(),
            updated_at: "invalid-date".to_string(),
            state: None,
            labels: LabelsConnection { nodes: vec![] },
            team: None,
            project: None,
            assignee: None,
        };

        let issue = source.map_issue(linear_issue);

        assert_eq!(issue.id, "123");
        assert_eq!(issue.short_id, "TEST-1");
        assert_eq!(issue.priority, IssuePriority::None);
        assert_eq!(issue.status, IssueStatus::Open);
        assert!(issue.description.is_none());
        assert!(issue.created_at.is_none()); // Invalid date
        assert!(issue.updated_at.is_none());
    }

    #[test]
    fn test_map_issue_completed_state() {
        let source = LinearSource::new(test_config());
        let linear_issue = create_linear_issue(
            "123",
            "TEST-1",
            "Done issue",
            3,
            "completed",
            "Done",
            vec![],
        );

        let issue = source.map_issue(linear_issue);
        assert_eq!(issue.status, IssueStatus::Resolved);
    }

    #[test]
    fn test_map_issue_canceled_state() {
        let source = LinearSource::new(test_config());
        let mut linear_issue = create_linear_issue(
            "123",
            "TEST-1",
            "Canceled issue",
            4,
            "canceled",
            "Canceled",
            vec![],
        );
        linear_issue.state = Some(LinearState {
            name: "Canceled".to_string(),
            state_type: "canceled".to_string(),
        });

        let issue = source.map_issue(linear_issue);
        assert_eq!(issue.status, IssueStatus::Resolved);
    }

    #[test]
    fn test_map_issue_all_priorities() {
        let source = LinearSource::new(test_config());

        for (priority_num, expected) in [
            (0, IssuePriority::None),
            (1, IssuePriority::Critical),
            (2, IssuePriority::High),
            (3, IssuePriority::Medium),
            (4, IssuePriority::Low),
            (5, IssuePriority::None),
        ] {
            let linear_issue = create_linear_issue(
                "123",
                "TEST-1",
                "Test",
                priority_num,
                "backlog",
                "Backlog",
                vec![],
            );
            let issue = source.map_issue(linear_issue);
            assert_eq!(
                issue.priority, expected,
                "Priority {} should map to {:?}",
                priority_num, expected
            );
        }
    }

    #[test]
    fn test_graphql_request_serialization() {
        let request = GraphQLRequest {
            query: "query { test }",
            variables: serde_json::json!({"id": "123"}),
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("query"));
        assert!(json.contains("variables"));
        assert!(json.contains("123"));
    }

    #[test]
    fn test_graphql_error_debug() {
        let error = GraphQLError {
            message: "Test error".to_string(),
        };
        let debug = format!("{:?}", error);
        assert!(debug.contains("Test error"));
    }

    #[test]
    fn test_linear_state_deserialization() {
        let json = r#"{"name": "In Progress", "type": "started"}"#;
        let state: LinearState = serde_json::from_str(json).unwrap();
        assert_eq!(state.name, "In Progress");
        assert_eq!(state.state_type, "started");
    }

    #[test]
    fn test_linear_label_deserialization() {
        let json = r#"{"name": "bug"}"#;
        let label: LinearLabel = serde_json::from_str(json).unwrap();
        assert_eq!(label.name, "bug");
    }

    #[test]
    fn test_linear_team_deserialization() {
        let json = r#"{"id": "team_123", "name": "Engineering"}"#;
        let team: LinearTeam = serde_json::from_str(json).unwrap();
        assert_eq!(team.id, "team_123");
        assert_eq!(team.name, "Engineering");
    }

    #[test]
    fn test_linear_project_deserialization() {
        let json = r#"{"id": "proj_456", "name": "Backend"}"#;
        let project: LinearProject = serde_json::from_str(json).unwrap();
        assert_eq!(project.id, "proj_456");
        assert_eq!(project.name, "Backend");
    }

    #[test]
    fn test_linear_user_deserialization() {
        let json = r#"{"name": "John Doe"}"#;
        let user: LinearUser = serde_json::from_str(json).unwrap();
        assert_eq!(user.name, "John Doe");
    }

    #[test]
    fn test_linear_issue_full_deserialization() {
        let json = r#"{
            "id": "issue_123",
            "identifier": "PROJ-123",
            "title": "Test issue",
            "description": "Description here",
            "url": "https://linear.app/test",
            "priority": 2,
            "createdAt": "2024-01-01T00:00:00Z",
            "updatedAt": "2024-01-02T00:00:00Z",
            "state": {"name": "Todo", "type": "unstarted"},
            "labels": {"nodes": [{"name": "bug"}, {"name": "p1"}]},
            "team": {"id": "t1", "name": "Team"},
            "project": {"id": "p1", "name": "Project"},
            "assignee": {"name": "Jane"}
        }"#;
        let issue: LinearIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.id, "issue_123");
        assert_eq!(issue.identifier, "PROJ-123");
        assert_eq!(issue.priority, 2);
        assert_eq!(issue.labels.nodes.len(), 2);
        assert!(issue.team.is_some());
        assert!(issue.project.is_some());
        assert!(issue.assignee.is_some());
    }

    #[test]
    fn test_issues_response_deserialization() {
        let json = r#"{
            "issues": {
                "nodes": [
                    {
                        "id": "1",
                        "identifier": "T-1",
                        "title": "Issue 1",
                        "description": null,
                        "url": "https://linear.app/1",
                        "priority": 3,
                        "createdAt": "2024-01-01T00:00:00Z",
                        "updatedAt": "2024-01-01T00:00:00Z",
                        "state": null,
                        "labels": {"nodes": []},
                        "team": null,
                        "project": null,
                        "assignee": null
                    }
                ]
            }
        }"#;
        let response: IssuesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.issues.nodes.len(), 1);
    }

    #[test]
    fn test_issue_response_deserialization() {
        let json = r#"{
            "issue": {
                "id": "1",
                "identifier": "T-1",
                "title": "Issue 1",
                "description": null,
                "url": "https://linear.app/1",
                "priority": 3,
                "createdAt": "2024-01-01T00:00:00Z",
                "updatedAt": "2024-01-01T00:00:00Z",
                "state": null,
                "labels": {"nodes": []},
                "team": null,
                "project": null,
                "assignee": null
            }
        }"#;
        let response: IssueResponse = serde_json::from_str(json).unwrap();
        assert!(response.issue.is_some());
    }

    #[test]
    fn test_issue_response_null_deserialization() {
        let json = r#"{"issue": null}"#;
        let response: IssueResponse = serde_json::from_str(json).unwrap();
        assert!(response.issue.is_none());
    }

    #[test]
    fn test_graphql_response_with_errors() {
        let json = r#"{
            "data": null,
            "errors": [{"message": "Error 1"}, {"message": "Error 2"}]
        }"#;
        let response: GraphQLResponse<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert!(response.data.is_none());
        assert!(response.errors.is_some());
        assert_eq!(response.errors.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_graphql_response_with_data() {
        let json = r#"{"data": {"test": "value"}, "errors": null}"#;
        let response: GraphQLResponse<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert!(response.data.is_some());
        assert!(response.errors.is_none());
    }

    #[test]
    fn test_map_issue_with_valid_dates() {
        let source = LinearSource::new(test_config());
        let linear_issue = LinearIssue {
            id: "123".to_string(),
            identifier: "TEST-1".to_string(),
            title: "Test".to_string(),
            description: None,
            url: "https://linear.app/test".to_string(),
            priority: 0,
            created_at: "2024-06-15T10:30:00.000Z".to_string(),
            updated_at: "2024-06-16T14:45:00.000Z".to_string(),
            state: None,
            labels: LabelsConnection { nodes: vec![] },
            team: None,
            project: None,
            assignee: None,
        };

        let issue = source.map_issue(linear_issue);
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());
    }

    #[test]
    fn test_queries_constant() {
        // Verify queries are valid strings
        assert!(ISSUES_QUERY.contains("query Issues"));
        assert!(ISSUES_QUERY.contains("nodes"));
        assert!(ISSUE_QUERY.contains("query Issue"));
        assert!(ISSUE_QUERY.contains("$id"));
    }

    #[tokio::test]
    async fn test_build_issue_context_with_all_metadata() {
        let source = LinearSource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "PROJ-456",
            "Complex Bug",
            "https://linear.app/123",
            "linear",
        );
        issue.description = Some("A very complex bug description\nwith multiple lines".to_string());
        issue.priority = IssuePriority::Critical;
        issue.status = IssueStatus::InProgress;
        issue.set_metadata("team", "Platform");
        issue.set_metadata("project", "Infrastructure");
        issue.set_metadata("assignee", "Alice Smith");

        let context = source.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("# Linear Issue: PROJ-456"));
        assert!(context.contains("**Title:** Complex Bug"));
        assert!(context.contains("https://linear.app/123"));
        assert!(context.contains("Priority")); // Check priority line exists
        assert!(context.contains("A very complex bug description"));
        assert!(context.contains("Platform"));
        assert!(context.contains("Infrastructure"));
        assert!(context.contains("Alice Smith"));
    }
}

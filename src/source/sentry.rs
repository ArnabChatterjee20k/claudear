//! Sentry issue source adapter.

use super::IssueSource;
use crate::config::SentryConfig;
use crate::error::{Error, Result};
use crate::types::{Issue, IssuePriority, IssueStatus, MatchPriority, MatchResult};
use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashSet;

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

/// Trait for HTTP client operations to enable testing.
#[async_trait]
pub trait SentryHttpClient: Send + Sync {
    /// Perform a GET request with bearer auth.
    async fn get(&self, url: &str, auth_token: &str) -> Result<HttpResponse>;

    /// Perform a PUT request with bearer auth and JSON body.
    async fn put(
        &self,
        url: &str,
        auth_token: &str,
        body: serde_json::Value,
    ) -> Result<HttpResponse>;
}

/// Default HTTP client using reqwest.
pub struct ReqwestSentryClient {
    client: reqwest::Client,
}

impl ReqwestSentryClient {
    /// Create a new reqwest-based HTTP client.
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestSentryClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SentryHttpClient for ReqwestSentryClient {
    async fn get(&self, url: &str, auth_token: &str) -> Result<HttpResponse> {
        let response = self
            .client
            .get(url)
            .bearer_auth(auth_token)
            .header("Content-Type", "application/json")
            .send()
            .await?;
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        Ok(HttpResponse { status, body })
    }

    async fn put(
        &self,
        url: &str,
        auth_token: &str,
        body: serde_json::Value,
    ) -> Result<HttpResponse> {
        let response = self
            .client
            .put(url)
            .header("Authorization", format!("Bearer {}", auth_token))
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

/// Sentry REST API client.
pub struct SentrySource<H: SentryHttpClient = ReqwestSentryClient> {
    config: SentryConfig,
    http: H,
    escalating_issue_ids: std::sync::RwLock<HashSet<String>>,
}

#[derive(Debug, Deserialize)]
struct SentryApiIssue {
    id: String,
    #[serde(rename = "shortId")]
    short_id: String,
    title: String,
    culprit: Option<String>,
    permalink: String,
    #[serde(rename = "firstSeen")]
    first_seen: String,
    #[serde(rename = "lastSeen")]
    last_seen: String,
    count: String,
    #[serde(rename = "userCount")]
    user_count: Option<i64>,
    project: SentryProject,
    status: String,
    level: String,
    #[serde(rename = "isUnhandled")]
    is_unhandled: Option<bool>,
    metadata: Option<SentryMetadata>,
    stats: Option<SentryStats>,
}

#[derive(Debug, Deserialize)]
struct SentryProject {
    name: String,
    slug: String,
}

#[derive(Debug, Deserialize)]
struct SentryMetadata {
    #[serde(rename = "type")]
    error_type: Option<String>,
    value: Option<String>,
    filename: Option<String>,
    function: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SentryStats {
    #[serde(rename = "24h")]
    last_24h: Option<Vec<(i64, i64)>>,
}

#[derive(Debug, Deserialize)]
struct SentryEvent {
    tags: Option<Vec<SentryTag>>,
    entries: Option<Vec<SentryEntry>>,
}

#[derive(Debug, Deserialize)]
struct SentryTag {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct SentryEntry {
    #[serde(rename = "type")]
    entry_type: String,
    data: serde_json::Value,
}

impl SentrySource<ReqwestSentryClient> {
    /// Create a new Sentry source with the default HTTP client.
    pub fn new(config: SentryConfig) -> Self {
        Self {
            config,
            http: ReqwestSentryClient::new(),
            escalating_issue_ids: std::sync::RwLock::new(HashSet::new()),
        }
    }
}

impl<H: SentryHttpClient> SentrySource<H> {
    /// Create a new Sentry source with a custom HTTP client.
    pub fn with_http_client(config: SentryConfig, http: H) -> Self {
        Self {
            config,
            http,
            escalating_issue_ids: std::sync::RwLock::new(HashSet::new()),
        }
    }

    async fn fetch<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T> {
        let url = format!("https://sentry.io/api/0{}", endpoint);
        let response = self.http.get(&url, &self.config.auth_token).await?;

        if !response.is_success() {
            return Err(Error::source(
                "sentry",
                format!("API error ({}): {}", response.status, response.body),
            ));
        }

        response.json()
    }

    async fn fetch_top_issues(&self) -> Result<Vec<SentryApiIssue>> {
        let mut query_parts = vec!["is:unresolved".to_string()];

        if !self.config.project_slugs.is_empty() {
            let project_query = self
                .config
                .project_slugs
                .iter()
                .map(|p| format!("project:{}", p))
                .collect::<Vec<_>>()
                .join(" OR ");
            query_parts.push(project_query);
        }

        let query = query_parts.join(" ");
        let endpoint = format!(
            "/organizations/{}/issues/?query={}&sort=freq&limit={}&statsPeriod={}",
            self.config.org_slug,
            urlencoding::encode(&query),
            self.config.top_issues_count,
            self.config.top_issues_period.to_stats_period()
        );

        self.fetch(&endpoint).await
    }

    async fn fetch_escalating_issues(&self) -> Result<Vec<SentryApiIssue>> {
        let mut query_parts = vec!["is:unresolved".to_string(), "is:escalating".to_string()];

        if !self.config.project_slugs.is_empty() {
            let project_query = self
                .config
                .project_slugs
                .iter()
                .map(|p| format!("project:{}", p))
                .collect::<Vec<_>>()
                .join(" OR ");
            query_parts.push(project_query);
        }

        let query = query_parts.join(" ");
        let endpoint = format!(
            "/organizations/{}/issues/?query={}&sort=date&limit=100",
            self.config.org_slug,
            urlencoding::encode(&query)
        );

        self.fetch(&endpoint).await
    }

    async fn fetch_latest_event(&self, issue_id: &str) -> Result<SentryEvent> {
        let endpoint = format!("/issues/{}/events/latest/", issue_id);
        self.fetch(&endpoint).await
    }

    fn map_issue(&self, api_issue: SentryApiIssue) -> Issue {
        let event_count: i64 = api_issue.count.parse().unwrap_or(0);
        let escalation_rate = self.calculate_escalation_rate(&api_issue);
        let is_escalating = self
            .escalating_issue_ids
            .read()
            .map(|ids| ids.contains(&api_issue.id))
            .unwrap_or(false);

        let mut issue = Issue::new(
            &api_issue.id,
            &api_issue.short_id,
            &api_issue.title,
            &api_issue.permalink,
            "sentry",
        );

        issue.description = api_issue.metadata.as_ref().and_then(|m| m.value.clone());
        issue.priority = Self::map_priority(&api_issue.level, event_count);
        issue.status = Self::map_status(&api_issue.status);

        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&api_issue.first_seen) {
            issue.created_at = Some(dt.with_timezone(&chrono::Utc));
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&api_issue.last_seen) {
            issue.updated_at = Some(dt.with_timezone(&chrono::Utc));
        }

        // Store metadata
        issue.set_metadata("culprit", api_issue.culprit.as_deref().unwrap_or(""));
        issue.set_metadata("level", &api_issue.level);
        issue.set_metadata("project", &api_issue.project.name);
        issue.set_metadata("project_slug", &api_issue.project.slug);
        issue.set_metadata("event_count", event_count);
        issue.set_metadata("user_count", api_issue.user_count.unwrap_or(0));
        issue.set_metadata("is_unhandled", api_issue.is_unhandled.unwrap_or(false));
        issue.set_metadata("is_escalating", is_escalating);

        if let Some(ref metadata) = api_issue.metadata {
            if let Some(ref t) = metadata.error_type {
                issue.set_metadata("error_type", t);
            }
            if let Some(ref v) = metadata.value {
                issue.set_metadata("error_value", v);
            }
            if let Some(ref f) = metadata.filename {
                issue.set_metadata("filename", f);
            }
            if let Some(ref f) = metadata.function {
                issue.set_metadata("function", f);
            }
        }

        if let Some(rate) = escalation_rate {
            issue.set_metadata("escalation_rate", rate);
        }

        issue
    }

    fn map_priority(level: &str, event_count: i64) -> IssuePriority {
        if level == "fatal" || (level == "error" && event_count > 1000) {
            IssuePriority::Critical
        } else if level == "error" {
            IssuePriority::High
        } else if level == "warning" {
            IssuePriority::Medium
        } else {
            IssuePriority::Low
        }
    }

    fn map_status(status: &str) -> IssueStatus {
        match status {
            "resolved" => IssueStatus::Resolved,
            "ignored" => IssueStatus::Ignored,
            _ => IssueStatus::Open,
        }
    }

    /// Check if a Sentry status represents a terminal/resolved state.
    /// Terminal states are those where the issue is considered "done" - no further action needed.
    pub fn is_issue_resolved(status: &str) -> bool {
        let s = status.to_lowercase();
        s == "resolved" || s == "ignored"
    }

    fn calculate_escalation_rate(&self, issue: &SentryApiIssue) -> Option<f64> {
        let stats = issue.stats.as_ref()?.last_24h.as_ref()?;

        if stats.len() < 4 {
            return None;
        }

        let midpoint = stats.len() / 2;
        let first_half: i64 = stats[..midpoint].iter().map(|(_, count)| count).sum();
        let second_half: i64 = stats[midpoint..].iter().map(|(_, count)| count).sum();

        if first_half == 0 {
            return Some(if second_half > 0 { 100.0 } else { 0.0 });
        }

        Some(((second_half - first_half) as f64 / first_half as f64) * 100.0)
    }
}

#[async_trait]
impl<H: SentryHttpClient + 'static> IssueSource for SentrySource<H> {
    fn name(&self) -> &str {
        "sentry"
    }

    fn display_name(&self) -> &str {
        "Sentry"
    }

    async fn fetch_issues(&self) -> Result<Vec<Issue>> {
        // Fetch both escalating and top issues
        let (escalating_result, top_result) =
            tokio::join!(self.fetch_escalating_issues(), self.fetch_top_issues());

        let escalating_issues = escalating_result.unwrap_or_else(|e| {
            tracing::warn!(source = "sentry", error = %e, "Failed to fetch escalating issues");
            vec![]
        });

        let top_issues = top_result?;

        // Track escalating IDs
        match self.escalating_issue_ids.write() {
            Ok(mut ids) => {
                ids.clear();
                for issue in &escalating_issues {
                    ids.insert(issue.id.clone());
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "escalating_issue_ids RwLock poisoned, skipping update");
            }
        }

        // Combine and dedupe
        let mut seen = HashSet::new();
        let mut all_issues = Vec::new();

        for issue in escalating_issues.into_iter().chain(top_issues) {
            if !seen.contains(&issue.id) {
                seen.insert(issue.id.clone());
                all_issues.push(self.map_issue(issue));
            }
        }

        Ok(all_issues)
    }

    fn matches_criteria(&self, issue: &Issue) -> MatchResult {
        let event_count: i64 = issue.get_metadata("event_count").unwrap_or(0);
        let is_escalating: bool = issue.get_metadata("is_escalating").unwrap_or(false);
        let escalation_rate: Option<f64> = issue.get_metadata("escalation_rate");

        // Check minimum event count
        if event_count < self.config.min_event_count as i64 {
            return MatchResult::not_matched(format!(
                "Event count {} below threshold {}",
                event_count, self.config.min_event_count
            ));
        }

        // Check if resolved
        if issue.status == IssueStatus::Resolved {
            return MatchResult::not_matched("Issue is already resolved");
        }

        // Determine priority and reason
        let (priority, reason) = if is_escalating {
            (
                MatchPriority::Urgent,
                "Issue is escalating (flagged by Sentry)".to_string(),
            )
        } else if let Some(rate) = escalation_rate {
            if rate >= self.config.escalation_threshold_percent as f64 {
                (
                    MatchPriority::Urgent,
                    format!("Issue is escalating ({:.1}% increase)", rate),
                )
            } else if issue.priority == IssuePriority::Critical
                || issue.priority == IssuePriority::High
            {
                (
                    MatchPriority::High,
                    format!("Top issue by frequency ({} events)", event_count),
                )
            } else {
                (
                    MatchPriority::Normal,
                    format!("Top issue by frequency ({} events)", event_count),
                )
            }
        } else if issue.priority == IssuePriority::Critical || issue.priority == IssuePriority::High
        {
            (
                MatchPriority::High,
                format!("Top issue by frequency ({} events)", event_count),
            )
        } else {
            (
                MatchPriority::Normal,
                format!("Top issue by frequency ({} events)", event_count),
            )
        };

        MatchResult::matched(reason, priority)
    }

    async fn build_issue_context(&self, issue: &Issue) -> Result<String> {
        let mut context = format!("# Sentry Issue: {}\n\n", issue.short_id);
        context.push_str(&format!("**Title:** {}\n", issue.title));
        context.push_str(&format!("**URL:** {}\n", issue.url));

        if let Some(level) = issue.get_metadata::<String>("level") {
            context.push_str(&format!("**Level:** {}\n", level));
        }

        context.push_str(&format!("**Status:** {}\n", issue.status));

        if let Some(event_count) = issue.get_metadata::<i64>("event_count") {
            context.push_str(&format!("**Event Count:** {}\n", event_count));
        }
        if let Some(user_count) = issue.get_metadata::<i64>("user_count") {
            context.push_str(&format!("**User Count:** {}\n", user_count));
        }
        if let Some(project) = issue.get_metadata::<String>("project") {
            context.push_str(&format!("**Project:** {}\n\n", project));
        }

        if let Some(culprit) = issue.get_metadata::<String>("culprit") {
            if !culprit.is_empty() {
                context.push_str(&format!("**Culprit:** {}\n\n", culprit));
            }
        }

        // Error details
        let error_type: Option<String> = issue.get_metadata("error_type");
        let error_value: Option<String> = issue.get_metadata("error_value");
        let filename: Option<String> = issue.get_metadata("filename");
        let function: Option<String> = issue.get_metadata("function");

        if error_type.is_some() || error_value.is_some() {
            context.push_str("## Error Details\n");
            if let Some(ref t) = error_type {
                context.push_str(&format!("- **Type:** {}\n", t));
            }
            if let Some(ref v) = error_value {
                context.push_str(&format!("- **Value:** {}\n", v));
            }
            if let Some(ref f) = filename {
                context.push_str(&format!("- **File:** {}\n", f));
            }
            if let Some(ref f) = function {
                context.push_str(&format!("- **Function:** {}\n", f));
            }
            context.push('\n');
        }

        // Try to get stack trace from latest event
        match self.fetch_latest_event(&issue.id).await {
            Ok(event) => {
                if let Some(entries) = event.entries {
                    if let Some(exception_entry) =
                        entries.iter().find(|e| e.entry_type == "exception")
                    {
                        if let Some(values) = exception_entry.data.get("values") {
                            if let Some(arr) = values.as_array() {
                                context.push_str("## Stack Trace\n```\n");
                                for exc in arr {
                                    if let (Some(exc_type), Some(exc_value)) =
                                        (exc.get("type"), exc.get("value"))
                                    {
                                        context.push_str(&format!(
                                            "{}: {}\n",
                                            exc_type.as_str().unwrap_or(""),
                                            exc_value.as_str().unwrap_or("")
                                        ));
                                    }
                                    if let Some(stacktrace) = exc.get("stacktrace") {
                                        if let Some(frames) = stacktrace.get("frames") {
                                            if let Some(frames_arr) = frames.as_array() {
                                                for frame in frames_arr.iter().rev().take(10) {
                                                    let func = frame
                                                        .get("function")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("<anonymous>");
                                                    let file = frame
                                                        .get("filename")
                                                        .and_then(|v| v.as_str())
                                                        .unwrap_or("?");
                                                    let line = frame
                                                        .get("lineNo")
                                                        .and_then(|v| v.as_i64())
                                                        .unwrap_or(0);
                                                    let col = frame
                                                        .get("colNo")
                                                        .and_then(|v| v.as_i64())
                                                        .unwrap_or(0);
                                                    context.push_str(&format!(
                                                        "  at {} ({}:{}:{})\n",
                                                        func, file, line, col
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                                context.push_str("```\n\n");
                            }
                        }
                    }
                }

                if let Some(tags) = event.tags {
                    context.push_str("## Tags\n");
                    for tag in tags.iter().take(20) {
                        context.push_str(&format!("- **{}:** {}\n", tag.key, tag.value));
                    }
                    context.push('\n');
                }
            }
            Err(e) => {
                tracing::warn!(
                    source = "sentry",
                    short_id = %issue.short_id,
                    error = %e,
                    "Failed to fetch event details"
                );
            }
        }

        Ok(context)
    }

    async fn get_issue(&self, issue_id: &str) -> Result<Issue> {
        let endpoint = format!("/issues/{}/", issue_id);
        let api_issue: SentryApiIssue = self.fetch(&endpoint).await?;
        Ok(self.map_issue(api_issue))
    }

    async fn resolve_issue(&self, issue_id: &str) -> Result<()> {
        let url = format!("https://sentry.io/api/0/issues/{}/", issue_id);

        let response = self
            .http
            .put(
                &url,
                &self.config.auth_token,
                serde_json::json!({
                    "status": "resolved"
                }),
            )
            .await?;

        if !response.is_success() {
            return Err(Error::source(
                "sentry",
                format!("Failed to resolve issue: {}", response.body),
            ));
        }

        tracing::info!(source = "sentry", issue_id = %issue_id, "Resolved issue");
        Ok(())
    }

    async fn get_issue_status(&self, issue_id: &str) -> Result<String> {
        let issue = self.get_issue(issue_id).await?;
        // Return the raw status string (e.g., "resolved", "ignored", "unresolved")
        Ok(format!("{:?}", issue.status).to_lowercase())
    }

    fn is_terminal_status(&self, status: &str) -> bool {
        Self::is_issue_resolved(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TopIssuesPeriod;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Mock HTTP client for testing.
    pub struct MockSentryClient {
        get_responses: Mutex<HashMap<String, HttpResponse>>,
        put_responses: Mutex<HashMap<String, HttpResponse>>,
        requests: Mutex<Vec<(String, String)>>, // (method, url)
    }

    impl MockSentryClient {
        pub fn new() -> Self {
            Self {
                get_responses: Mutex::new(HashMap::new()),
                put_responses: Mutex::new(HashMap::new()),
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

        pub fn mock_put(&self, url: impl Into<String>, status: u16, body: impl Into<String>) {
            let mut responses = self.put_responses.lock().unwrap();
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
    impl SentryHttpClient for MockSentryClient {
        async fn get(&self, url: &str, _auth_token: &str) -> Result<HttpResponse> {
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

        async fn put(
            &self,
            url: &str,
            _auth_token: &str,
            _body: serde_json::Value,
        ) -> Result<HttpResponse> {
            self.requests
                .lock()
                .unwrap()
                .push(("PUT".to_string(), url.to_string()));
            let responses = self.put_responses.lock().unwrap();
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

    #[test]
    fn test_http_response_json() {
        let response = HttpResponse {
            status: 200,
            body: r#"{"id": "123"}"#.to_string(),
        };
        let parsed: serde_json::Value = response.json().unwrap();
        assert_eq!(parsed["id"], "123");
    }

    #[tokio::test]
    async fn test_fetch_issues_success() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=date&limit=100",
            200,
            "[]",
        );
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=freq&limit=100&statsPeriod=24h",
            200,
            r#"[{
                "id": "123",
                "shortId": "SENTRY-123",
                "title": "Test Error",
                "culprit": "app.js",
                "permalink": "https://sentry.io/issue/123",
                "firstSeen": "2024-01-01T00:00:00Z",
                "lastSeen": "2024-01-02T00:00:00Z",
                "count": "100",
                "userCount": 10,
                "project": {"id": "1", "name": "Test Project", "slug": "test"},
                "status": "unresolved",
                "level": "error"
            }]"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].short_id, "SENTRY-123");
        assert_eq!(issues[0].title, "Test Error");
    }

    #[tokio::test]
    async fn test_fetch_issues_api_error() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=date&limit=100",
            200,
            "[]",
        );
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=freq&limit=100&statsPeriod=24h",
            500,
            "Internal Server Error",
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let result = source.fetch_issues().await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_issue_success() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/issues/123/",
            200,
            r#"{
                "id": "123",
                "shortId": "SENTRY-123",
                "title": "Test Error",
                "permalink": "https://sentry.io/issue/123",
                "firstSeen": "2024-01-01T00:00:00Z",
                "lastSeen": "2024-01-02T00:00:00Z",
                "count": "50",
                "project": {"id": "1", "name": "Test", "slug": "test"},
                "status": "unresolved",
                "level": "warning"
            }"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let issue = source.get_issue("123").await.unwrap();

        assert_eq!(issue.id, "123");
        assert_eq!(issue.short_id, "SENTRY-123");
    }

    #[tokio::test]
    async fn test_get_issue_not_found() {
        let mock = MockSentryClient::new();
        // No mock response means 404

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let result = source.get_issue("nonexistent").await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_issue_success() {
        let mock = MockSentryClient::new();
        mock.mock_put(
            "https://sentry.io/api/0/issues/123/",
            200,
            r#"{"status": "resolved"}"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let result = source.resolve_issue("123").await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_issue_failure() {
        let mock = MockSentryClient::new();
        mock.mock_put("https://sentry.io/api/0/issues/123/", 403, "Forbidden");

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let result = source.resolve_issue("123").await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to resolve"));
    }

    #[tokio::test]
    async fn test_build_issue_context() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/issues/123/events/latest/",
            200,
            r#"{
                "eventID": "event-1",
                "title": "Test Error",
                "dateCreated": "2024-01-01T00:00:00Z",
                "tags": [{"key": "environment", "value": "production"}],
                "entries": [{
                    "type": "exception",
                    "data": {
                        "values": [{
                            "type": "TypeError",
                            "value": "Cannot read property",
                            "stacktrace": {
                                "frames": [{
                                    "function": "main",
                                    "filename": "app.js",
                                    "lineNo": 42,
                                    "colNo": 10
                                }]
                            }
                        }]
                    }
                }]
            }"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Test Error",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("project", "Test Project");
        issue.set_metadata("culprit", "app.js:main");
        issue.set_metadata("error_type", "TypeError");
        issue.set_metadata("error_value", "Cannot read property");

        let context = source.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("Test Project"));
        assert!(context.contains("TypeError"));
        assert!(context.contains("Stack Trace"));
        assert!(context.contains("environment"));
    }

    #[tokio::test]
    async fn test_build_issue_context_empty_culprit() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/issues/456/events/latest/",
            200,
            r#"{
                "eventID": "event-2",
                "title": "Warning",
                "dateCreated": "2024-01-01T00:00:00Z"
            }"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);

        let mut issue = Issue::new(
            "456",
            "SENTRY-456",
            "Warning",
            "https://sentry.io/issue/456",
            "sentry",
        );
        issue.set_metadata("culprit", ""); // Empty culprit

        let context = source.build_issue_context(&issue).await.unwrap();

        // Should not contain "Culprit:" for empty culprit
        assert!(!context.contains("**Culprit:**"));
    }

    #[tokio::test]
    async fn test_build_issue_context_minimal() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/issues/789/events/latest/",
            200,
            r#"{
                "eventID": "event-3",
                "title": "Basic",
                "dateCreated": "2024-01-01T00:00:00Z"
            }"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);

        let issue = Issue::new(
            "789",
            "SENTRY-789",
            "Basic Issue",
            "https://sentry.io/issue/789",
            "sentry",
        );

        let context = source.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("Basic Issue"));
        assert!(context.contains("https://sentry.io/issue/789"));
    }

    #[tokio::test]
    async fn test_build_issue_context_with_all_metadata() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/issues/999/events/latest/",
            200,
            r#"{
                "eventID": "event-4",
                "title": "Full Error",
                "dateCreated": "2024-01-01T00:00:00Z",
                "tags": [
                    {"key": "browser", "value": "Chrome"},
                    {"key": "os", "value": "Windows"}
                ]
            }"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);

        let mut issue = Issue::new(
            "999",
            "SENTRY-999",
            "Full Error",
            "https://sentry.io/issue/999",
            "sentry",
        );
        issue.set_metadata("project", "Full Project");
        issue.set_metadata("culprit", "handler.js:processRequest");
        issue.set_metadata("error_type", "ReferenceError");
        issue.set_metadata("error_value", "x is not defined");
        issue.set_metadata("filename", "handler.js");
        issue.set_metadata("function", "processRequest");

        let context = source.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("Full Project"));
        assert!(context.contains("ReferenceError"));
        assert!(context.contains("x is not defined"));
        assert!(context.contains("handler.js"));
        assert!(context.contains("processRequest"));
        assert!(context.contains("Chrome"));
        assert!(context.contains("Windows"));
    }

    #[tokio::test]
    async fn test_fetch_escalating_issues() {
        let mock = MockSentryClient::new();
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved%20is%3Aescalating&sort=date&limit=100",
            200,
            r#"[{
                "id": "escalating-1",
                "shortId": "ESC-1",
                "title": "Escalating Error",
                "permalink": "https://sentry.io/issue/escalating-1",
                "firstSeen": "2024-01-01T00:00:00Z",
                "lastSeen": "2024-01-02T00:00:00Z",
                "count": "500",
                "project": {"id": "1", "name": "Test", "slug": "test"},
                "status": "unresolved",
                "level": "error"
            }]"#,
        );
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=freq&limit=100&statsPeriod=24h",
            200,
            "[]",
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].short_id, "ESC-1");

        // Check that escalating flag is set
        let is_escalating: bool = issues[0].get_metadata("is_escalating").unwrap_or(false);
        assert!(is_escalating);
    }

    #[tokio::test]
    async fn test_fetch_deduplicates_issues() {
        let mock = MockSentryClient::new();
        // Same issue appears in both escalating and top issues
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved%20is%3Aescalating&sort=date&limit=100",
            200,
            r#"[{
                "id": "dupe-1",
                "shortId": "DUPE-1",
                "title": "Duplicate Issue",
                "permalink": "https://sentry.io/issue/dupe-1",
                "firstSeen": "2024-01-01T00:00:00Z",
                "lastSeen": "2024-01-02T00:00:00Z",
                "count": "100",
                "project": {"id": "1", "name": "Test", "slug": "test"},
                "status": "unresolved",
                "level": "error"
            }]"#,
        );
        mock.mock_get(
            "https://sentry.io/api/0/organizations/test-org/issues/?query=is%3Aunresolved&sort=freq&limit=100&statsPeriod=24h",
            200,
            r#"[{
                "id": "dupe-1",
                "shortId": "DUPE-1",
                "title": "Duplicate Issue",
                "permalink": "https://sentry.io/issue/dupe-1",
                "firstSeen": "2024-01-01T00:00:00Z",
                "lastSeen": "2024-01-02T00:00:00Z",
                "count": "100",
                "project": {"id": "1", "name": "Test", "slug": "test"},
                "status": "unresolved",
                "level": "error"
            }]"#,
        );

        let config = test_config();
        let source = SentrySource::with_http_client(config, mock);
        let issues = source.fetch_issues().await.unwrap();

        // Should only have 1 issue (deduplicated)
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn test_calculate_escalation_rate() {
        let source = SentrySource::new(test_config());

        // Test with escalating data (second half has more events)
        let issue_escalating = SentryApiIssue {
            id: "1".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Test".to_string(),
            culprit: None,
            permalink: "url".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-02T00:00:00Z".to_string(),
            count: "100".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(SentryStats {
                last_24h: Some(vec![(0, 10), (1, 10), (2, 30), (3, 40)]),
            }),
        };

        let rate = source.calculate_escalation_rate(&issue_escalating);
        assert!(rate.is_some());
        assert!(rate.unwrap() > 0.0);

        // Test with no stats
        let issue_no_stats = SentryApiIssue {
            id: "2".to_string(),
            short_id: "TEST-2".to_string(),
            title: "Test".to_string(),
            culprit: None,
            permalink: "url".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-02T00:00:00Z".to_string(),
            count: "100".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: None,
        };

        let rate = source.calculate_escalation_rate(&issue_no_stats);
        assert!(rate.is_none());

        // Test with insufficient stats
        let issue_few_stats = SentryApiIssue {
            id: "3".to_string(),
            short_id: "TEST-3".to_string(),
            title: "Test".to_string(),
            culprit: None,
            permalink: "url".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-02T00:00:00Z".to_string(),
            count: "100".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(SentryStats {
                last_24h: Some(vec![(0, 10)]),
            }),
        };

        let rate = source.calculate_escalation_rate(&issue_few_stats);
        assert!(rate.is_none());
    }

    #[test]
    fn test_calculate_escalation_rate_zero_first_half() {
        let source = SentrySource::new(test_config());

        let issue = SentryApiIssue {
            id: "1".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Test".to_string(),
            culprit: None,
            permalink: "url".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-02T00:00:00Z".to_string(),
            count: "100".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(SentryStats {
                last_24h: Some(vec![(0, 0), (1, 0), (2, 10), (3, 20)]),
            }),
        };

        let rate = source.calculate_escalation_rate(&issue);
        assert!(rate.is_some());
        assert_eq!(rate.unwrap(), 100.0); // When first half is 0, should return 100%
    }

    fn test_config() -> SentryConfig {
        SentryConfig {
            enabled: true,
            auth_token: "test_token".to_string(),
            org_slug: "test-org".to_string(),
            project_slugs: vec![],
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::OneDay,
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            ..Default::default()
        }
    }

    #[test]
    fn test_map_priority() {
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("fatal", 0),
            IssuePriority::Critical
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 1001),
            IssuePriority::Critical
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 100),
            IssuePriority::High
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("warning", 100),
            IssuePriority::Medium
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("info", 100),
            IssuePriority::Low
        );
    }

    #[test]
    fn test_map_status() {
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("resolved"),
            IssueStatus::Resolved
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("ignored"),
            IssueStatus::Ignored
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("unresolved"),
            IssueStatus::Open
        );
    }

    #[test]
    fn test_matches_criteria_min_event_count() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "TypeError",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 5i64);

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(result.reason.contains("below threshold"));
    }

    #[test]
    fn test_matches_criteria_resolved() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "TypeError",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.status = IssueStatus::Resolved;

        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
        assert!(result.reason.contains("resolved"));
    }

    #[test]
    fn test_matches_criteria_escalating() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "TypeError",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.set_metadata("is_escalating", true);

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
        assert!(result.reason.contains("escalating"));
    }

    #[test]
    fn test_matches_criteria_high_escalation_rate() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "TypeError",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.set_metadata("escalation_rate", 75.0);

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);
    }

    #[test]
    fn test_source_name() {
        let source = SentrySource::new(test_config());
        assert_eq!(source.name(), "sentry");
        assert_eq!(source.display_name(), "Sentry");
    }

    #[test]
    fn test_matches_criteria_normal_priority() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Warning message",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 50i64);
        issue.priority = IssuePriority::Medium;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
    }

    #[test]
    fn test_matches_criteria_high_priority_issue() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Error message",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 500i64);
        issue.priority = IssuePriority::High;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::High);
    }

    #[test]
    fn test_matches_criteria_critical_priority() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Fatal error",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 1000i64);
        issue.priority = IssuePriority::Critical;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::High);
    }

    #[test]
    fn test_matches_criteria_low_escalation_rate() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "TypeError",
            "https://sentry.io/issue/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.set_metadata("escalation_rate", 25.0); // Below threshold of 50

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        // Should not be Urgent because rate is below threshold
        assert_ne!(result.priority, MatchPriority::Urgent);
    }

    #[test]
    fn test_map_priority_all_levels() {
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("fatal", 0),
            IssuePriority::Critical
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("fatal", 1),
            IssuePriority::Critical
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 1001),
            IssuePriority::Critical
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 1000),
            IssuePriority::High
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 999),
            IssuePriority::High
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("error", 1),
            IssuePriority::High
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("warning", 0),
            IssuePriority::Medium
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("warning", 10000),
            IssuePriority::Medium
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("info", 0),
            IssuePriority::Low
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("debug", 0),
            IssuePriority::Low
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_priority("unknown", 0),
            IssuePriority::Low
        );
    }

    #[test]
    fn test_map_status_all_values() {
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("resolved"),
            IssueStatus::Resolved
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("ignored"),
            IssueStatus::Ignored
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("unresolved"),
            IssueStatus::Open
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status("reprocessing"),
            IssueStatus::Open
        );
        assert_eq!(
            SentrySource::<ReqwestSentryClient>::map_status(""),
            IssueStatus::Open
        );
    }

    #[test]
    fn test_config_with_project_filters() {
        let config = SentryConfig {
            enabled: true,
            auth_token: "test_token".to_string(),
            org_slug: "test-org".to_string(),
            project_slugs: vec!["frontend".to_string(), "backend".to_string()],
            top_issues_count: 50,
            top_issues_period: TopIssuesPeriod::OneDay,
            min_event_count: 5,
            escalation_threshold_percent: 25,
            client_secret: Some("secret".to_string()),
            ..Default::default()
        };
        let source = SentrySource::new(config);
        assert_eq!(source.name(), "sentry");
    }

    #[test]
    fn test_matches_criteria_threshold_boundary() {
        let config = SentryConfig {
            enabled: true,
            auth_token: "test_token".to_string(),
            org_slug: "test-org".to_string(),
            project_slugs: vec![],
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::OneDay,
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            ..Default::default()
        };
        let source = SentrySource::new(config);

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );

        // Exactly at threshold
        issue.set_metadata("event_count", 10i64);
        let result = source.matches_criteria(&issue);
        assert!(result.matches);

        // One below threshold
        issue.set_metadata("event_count", 9i64);
        let result = source.matches_criteria(&issue);
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_exact_escalation_threshold() {
        let config = SentryConfig {
            enabled: true,
            auth_token: "test_token".to_string(),
            org_slug: "test-org".to_string(),
            project_slugs: vec![],
            top_issues_count: 100,
            top_issues_period: TopIssuesPeriod::OneDay,
            min_event_count: 10,
            escalation_threshold_percent: 50,
            client_secret: None,
            ..Default::default()
        };
        let source = SentrySource::new(config);

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);

        // Exactly at escalation threshold
        issue.set_metadata("escalation_rate", 50.0);
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Urgent);

        // Just below escalation threshold
        issue.set_metadata("escalation_rate", 49.9);
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_ne!(result.priority, MatchPriority::Urgent);
    }

    #[test]
    fn test_matches_criteria_no_metadata() {
        let source = SentrySource::new(test_config());

        let issue = Issue::new(
            "123",
            "SENTRY-123",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );

        let result = source.matches_criteria(&issue);
        // Should not match because event_count defaults to 0, which is below threshold
        assert!(!result.matches);
    }

    #[test]
    fn test_matches_criteria_ignored_status() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-123",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.status = IssueStatus::Ignored;

        // Ignored status is not resolved, so should match
        let result = source.matches_criteria(&issue);
        assert!(result.matches);
    }

    #[test]
    fn test_calculate_escalation_rate_increasing() {
        let source = SentrySource::new(test_config());

        // Stats showing increase over time (first half: 10, second half: 20 = 100% increase)
        let stats = SentryStats {
            last_24h: Some(vec![
                (0, 2),
                (1, 2),
                (2, 3),
                (3, 3), // First half: 10
                (4, 5),
                (5, 5),
                (6, 5),
                (7, 5), // Second half: 20
            ]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "SENTRY-123".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "30".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue).unwrap();
        assert!((rate - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_calculate_escalation_rate_decreasing() {
        let source = SentrySource::new(test_config());

        // Stats showing decrease (first half: 20, second half: 10 = -50%)
        let stats = SentryStats {
            last_24h: Some(vec![
                (0, 5),
                (1, 5),
                (2, 5),
                (3, 5), // First half: 20
                (4, 2),
                (5, 3),
                (6, 2),
                (7, 3), // Second half: 10
            ]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "SENTRY-123".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "30".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue).unwrap();
        assert!((rate - (-50.0)).abs() < 0.1);
    }

    #[test]
    fn test_calculate_escalation_rate_first_half_zero() {
        let source = SentrySource::new(test_config());

        // First half is zero (issue just started)
        let stats = SentryStats {
            last_24h: Some(vec![
                (0, 0),
                (1, 0),
                (2, 0),
                (3, 0), // First half: 0
                (4, 5),
                (5, 5),
                (6, 5),
                (7, 5), // Second half: 20
            ]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "SENTRY-123".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "20".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue).unwrap();
        assert_eq!(rate, 100.0); // New issue = 100%
    }

    #[test]
    fn test_calculate_escalation_rate_no_stats() {
        let source = SentrySource::new(test_config());

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "SENTRY-123".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "20".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: None,
        };

        let rate = source.calculate_escalation_rate(&issue);
        assert!(rate.is_none());
    }

    #[test]
    fn test_calculate_escalation_rate_insufficient_data() {
        let source = SentrySource::new(test_config());

        // Less than 4 data points
        let stats = SentryStats {
            last_24h: Some(vec![(0, 5), (1, 10), (2, 15)]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "SENTRY-123".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "30".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue);
        assert!(rate.is_none());
    }

    fn create_sentry_api_issue(
        id: &str,
        short_id: &str,
        title: &str,
        level: &str,
        status: &str,
        count: &str,
    ) -> SentryApiIssue {
        SentryApiIssue {
            id: id.to_string(),
            short_id: short_id.to_string(),
            title: title.to_string(),
            culprit: Some("src/app.js".to_string()),
            permalink: format!("https://sentry.io/issues/{}", id),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-02T00:00:00Z".to_string(),
            count: count.to_string(),
            user_count: Some(50),
            project: SentryProject {
                name: "Frontend".to_string(),
                slug: "frontend".to_string(),
            },
            status: status.to_string(),
            level: level.to_string(),
            is_unhandled: Some(true),
            metadata: Some(SentryMetadata {
                error_type: Some("TypeError".to_string()),
                value: Some("Cannot read property 'x' of undefined".to_string()),
                filename: Some("src/components/App.js".to_string()),
                function: Some("handleClick".to_string()),
            }),
            stats: None,
        }
    }

    #[test]
    fn test_map_issue_full() {
        let source = SentrySource::new(test_config());
        let api_issue = create_sentry_api_issue(
            "123456",
            "FRONTEND-ABC",
            "TypeError: Cannot read property",
            "error",
            "unresolved",
            "500",
        );

        let issue = source.map_issue(api_issue);

        assert_eq!(issue.id, "123456");
        assert_eq!(issue.short_id, "FRONTEND-ABC");
        assert_eq!(issue.title, "TypeError: Cannot read property");
        assert_eq!(issue.source, "sentry");
        assert_eq!(issue.priority, IssuePriority::High);
        assert_eq!(issue.status, IssueStatus::Open);
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());

        // Check metadata
        let culprit: Option<String> = issue.get_metadata("culprit");
        assert_eq!(culprit, Some("src/app.js".to_string()));

        let project: Option<String> = issue.get_metadata("project");
        assert_eq!(project, Some("Frontend".to_string()));

        let event_count: i64 = issue.get_metadata("event_count").unwrap_or(0);
        assert_eq!(event_count, 500);

        let user_count: i64 = issue.get_metadata("user_count").unwrap_or(0);
        assert_eq!(user_count, 50);

        let is_unhandled: bool = issue.get_metadata("is_unhandled").unwrap_or(false);
        assert!(is_unhandled);

        let error_type: Option<String> = issue.get_metadata("error_type");
        assert_eq!(error_type, Some("TypeError".to_string()));
    }

    #[test]
    fn test_map_issue_minimal() {
        let source = SentrySource::new(test_config());
        let api_issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Simple error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "invalid-date".to_string(),
            last_seen: "invalid-date".to_string(),
            count: "invalid".to_string(), // Invalid count
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "info".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: None,
        };

        let issue = source.map_issue(api_issue);

        assert_eq!(issue.id, "123");
        assert_eq!(issue.priority, IssuePriority::Low);
        assert!(issue.created_at.is_none()); // Invalid date
        assert!(issue.updated_at.is_none());

        let event_count: i64 = issue.get_metadata("event_count").unwrap_or(-1);
        assert_eq!(event_count, 0); // Invalid count parsed to 0
    }

    #[test]
    fn test_map_issue_resolved_status() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "resolved", "100");

        let issue = source.map_issue(api_issue);
        assert_eq!(issue.status, IssueStatus::Resolved);
    }

    #[test]
    fn test_map_issue_ignored_status() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "ignored", "100");

        let issue = source.map_issue(api_issue);
        assert_eq!(issue.status, IssueStatus::Ignored);
    }

    #[test]
    fn test_map_issue_fatal_level() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Fatal error", "fatal", "unresolved", "1");

        let issue = source.map_issue(api_issue);
        assert_eq!(issue.priority, IssuePriority::Critical);
    }

    #[test]
    fn test_map_issue_high_count_error() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "unresolved", "5000");

        let issue = source.map_issue(api_issue);
        assert_eq!(issue.priority, IssuePriority::Critical);
    }

    #[test]
    fn test_map_issue_warning_level() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Warning", "warning", "unresolved", "100");

        let issue = source.map_issue(api_issue);
        assert_eq!(issue.priority, IssuePriority::Medium);
    }

    #[test]
    fn test_map_issue_escalating() {
        let source = SentrySource::new(test_config());

        // Mark as escalating
        {
            let mut ids = source.escalating_issue_ids.write().unwrap();
            ids.insert("escalating_123".to_string());
        }

        let api_issue = create_sentry_api_issue(
            "escalating_123",
            "TEST-1",
            "Error",
            "error",
            "unresolved",
            "100",
        );

        let issue = source.map_issue(api_issue);
        let is_escalating: bool = issue.get_metadata("is_escalating").unwrap_or(false);
        assert!(is_escalating);
    }

    #[test]
    fn test_map_issue_with_escalation_rate() {
        let source = SentrySource::new(test_config());

        let mut api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "unresolved", "100");
        api_issue.stats = Some(SentryStats {
            last_24h: Some(vec![
                (0, 10),
                (1, 10),
                (2, 10),
                (3, 10), // First half: 40
                (4, 20),
                (5, 20),
                (6, 20),
                (7, 20), // Second half: 80
            ]),
        });

        let issue = source.map_issue(api_issue);
        let rate: Option<f64> = issue.get_metadata("escalation_rate");
        assert!(rate.is_some());
        assert!((rate.unwrap() - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_sentry_project_deserialization() {
        let json = r#"{"id": "123", "name": "My Project", "slug": "my-project"}"#;
        let project: SentryProject = serde_json::from_str(json).unwrap();
        assert_eq!(project.name, "My Project");
        assert_eq!(project.slug, "my-project");
    }

    #[test]
    fn test_sentry_metadata_deserialization() {
        let json = r#"{
            "type": "TypeError",
            "value": "Cannot read property",
            "filename": "app.js",
            "function": "onClick"
        }"#;
        let metadata: SentryMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.error_type, Some("TypeError".to_string()));
        assert_eq!(metadata.value, Some("Cannot read property".to_string()));
        assert_eq!(metadata.filename, Some("app.js".to_string()));
        assert_eq!(metadata.function, Some("onClick".to_string()));
    }

    #[test]
    fn test_sentry_metadata_partial_deserialization() {
        let json = r#"{"type": "Error"}"#;
        let metadata: SentryMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(metadata.error_type, Some("Error".to_string()));
        assert!(metadata.value.is_none());
        assert!(metadata.filename.is_none());
        assert!(metadata.function.is_none());
    }

    #[test]
    fn test_sentry_stats_deserialization() {
        let json = r#"{"24h": [[1704067200, 10], [1704070800, 20], [1704074400, 30]]}"#;
        let stats: SentryStats = serde_json::from_str(json).unwrap();
        assert!(stats.last_24h.is_some());
        assert_eq!(stats.last_24h.as_ref().unwrap().len(), 3);
    }

    #[test]
    fn test_sentry_tag_deserialization() {
        let json = r#"{"key": "browser", "value": "Chrome"}"#;
        let tag: SentryTag = serde_json::from_str(json).unwrap();
        assert_eq!(tag.key, "browser");
        assert_eq!(tag.value, "Chrome");
    }

    #[test]
    fn test_sentry_entry_deserialization() {
        let json = r#"{"type": "exception", "data": {"values": []}}"#;
        let entry: SentryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.entry_type, "exception");
        assert!(entry.data.is_object());
    }

    #[test]
    fn test_sentry_event_deserialization() {
        let json = r#"{
            "eventID": "abc123",
            "title": "TypeError",
            "message": "Error message",
            "dateCreated": "2024-01-01T00:00:00Z",
            "tags": [{"key": "browser", "value": "Chrome"}],
            "entries": []
        }"#;
        let event: SentryEvent = serde_json::from_str(json).unwrap();
        assert!(event.tags.is_some());
        assert!(event.entries.is_some());
    }

    #[test]
    fn test_sentry_api_issue_full_deserialization() {
        let json = r#"{
            "id": "123",
            "shortId": "PROJ-ABC",
            "title": "TypeError",
            "culprit": "app.js",
            "permalink": "https://sentry.io/123",
            "firstSeen": "2024-01-01T00:00:00Z",
            "lastSeen": "2024-01-02T00:00:00Z",
            "count": "1000",
            "userCount": 50,
            "project": {"id": "1", "name": "Test", "slug": "test"},
            "status": "unresolved",
            "level": "error",
            "isUnhandled": true,
            "metadata": {"type": "TypeError", "value": "error message"},
            "stats": {"24h": [[0, 10], [1, 20]]}
        }"#;
        let issue: SentryApiIssue = serde_json::from_str(json).unwrap();
        assert_eq!(issue.id, "123");
        assert_eq!(issue.short_id, "PROJ-ABC");
        assert_eq!(issue.count, "1000");
        assert_eq!(issue.user_count, Some(50));
        assert!(issue.is_unhandled.unwrap());
        assert!(issue.metadata.is_some());
        assert!(issue.stats.is_some());
    }

    #[test]
    fn test_calculate_escalation_rate_both_halves_zero() {
        let source = SentrySource::new(test_config());

        let stats = SentryStats {
            last_24h: Some(vec![
                (0, 0),
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 0),
                (7, 0),
            ]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "0".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue).unwrap();
        assert_eq!(rate, 0.0);
    }

    #[test]
    fn test_calculate_escalation_rate_exactly_four_points() {
        let source = SentrySource::new(test_config());

        let stats = SentryStats {
            last_24h: Some(vec![
                (0, 5),
                (1, 5), // First half: 10
                (2, 10),
                (3, 10), // Second half: 20
            ]),
        };

        let issue = SentryApiIssue {
            id: "123".to_string(),
            short_id: "TEST-1".to_string(),
            title: "Error".to_string(),
            culprit: None,
            permalink: "https://sentry.io/123".to_string(),
            first_seen: "2024-01-01T00:00:00Z".to_string(),
            last_seen: "2024-01-01T12:00:00Z".to_string(),
            count: "30".to_string(),
            user_count: None,
            project: SentryProject {
                name: "Test".to_string(),
                slug: "test".to_string(),
            },
            status: "unresolved".to_string(),
            level: "error".to_string(),
            is_unhandled: None,
            metadata: None,
            stats: Some(stats),
        };

        let rate = source.calculate_escalation_rate(&issue).unwrap();
        assert!((rate - 100.0).abs() < 0.1);
    }

    #[test]
    fn test_map_issue_with_valid_dates() {
        let source = SentrySource::new(test_config());
        let mut api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "unresolved", "100");
        api_issue.first_seen = "2024-06-15T10:30:00.000Z".to_string();
        api_issue.last_seen = "2024-06-16T14:45:00.000Z".to_string();

        let issue = source.map_issue(api_issue);
        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());
    }

    #[test]
    fn test_map_issue_metadata_all_fields() {
        let source = SentrySource::new(test_config());
        let api_issue =
            create_sentry_api_issue("123", "TEST-1", "Error", "error", "unresolved", "100");

        let issue = source.map_issue(api_issue);

        // Verify all metadata fields
        assert!(issue.get_metadata::<String>("culprit").is_some());
        assert!(issue.get_metadata::<String>("level").is_some());
        assert!(issue.get_metadata::<String>("project").is_some());
        assert!(issue.get_metadata::<String>("project_slug").is_some());
        assert!(issue.get_metadata::<i64>("event_count").is_some());
        assert!(issue.get_metadata::<i64>("user_count").is_some());
        assert!(issue.get_metadata::<bool>("is_unhandled").is_some());
        assert!(issue.get_metadata::<bool>("is_escalating").is_some());
        assert!(issue.get_metadata::<String>("error_type").is_some());
        assert!(issue.get_metadata::<String>("error_value").is_some());
        assert!(issue.get_metadata::<String>("filename").is_some());
        assert!(issue.get_metadata::<String>("function").is_some());
    }

    #[test]
    fn test_matches_criteria_with_escalation_rate_high_priority() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-1",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.set_metadata("escalation_rate", 30.0); // Below threshold
        issue.priority = IssuePriority::High;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::High);
    }

    #[test]
    fn test_matches_criteria_with_escalation_rate_critical_priority() {
        let source = SentrySource::new(test_config());

        let mut issue = Issue::new(
            "123",
            "SENTRY-1",
            "Error",
            "https://sentry.io/123",
            "sentry",
        );
        issue.set_metadata("event_count", 100i64);
        issue.set_metadata("escalation_rate", 30.0); // Below threshold
        issue.priority = IssuePriority::Critical;

        let result = source.matches_criteria(&issue);
        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::High);
    }
}

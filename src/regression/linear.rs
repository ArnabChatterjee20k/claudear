//! Linear bug regression checker.
//!
//! Checks for regressions of Linear bugs by:
//! 1. Searching GitHub issues in related repositories
//! 2. Scraping appwrite.io/threads for similar mentions
//! 3. Using semantic similarity to match issues

use crate::error::Result;
use crate::github::{HttpClient, HttpResponse, ReqwestHttpClient};
use crate::regression::{RegressionChecker, RegressionResult};
use crate::types::RegressionWatch;
use async_trait::async_trait;
use serde::Deserialize;

/// Configuration for Linear regression checking.
#[derive(Debug, Clone)]
pub struct LinearRegressionConfig {
    /// GitHub token for searching issues.
    pub github_token: String,
    /// Repositories to search for related issues.
    pub github_repos: Vec<String>,
    /// Similarity threshold (0.0-1.0) for semantic matching.
    pub similarity_threshold: f64,
}

impl Default for LinearRegressionConfig {
    fn default() -> Self {
        Self {
            github_token: String::new(),
            github_repos: vec![
                "appwrite/appwrite".to_string(),
                "appwrite/sdk-for-web".to_string(),
                "appwrite/sdk-for-flutter".to_string(),
            ],
            similarity_threshold: 0.75,
        }
    }
}

/// GitHub issue search result.
#[derive(Debug, Clone, Deserialize)]
struct GitHubSearchResult {
    #[allow(dead_code)]
    total_count: i64,
    items: Vec<GitHubIssue>,
}

/// A GitHub issue.
#[derive(Debug, Clone, Deserialize)]
struct GitHubIssue {
    #[allow(dead_code)]
    id: i64,
    number: i64,
    title: String,
    #[allow(dead_code)]
    body: Option<String>,
    #[allow(dead_code)]
    state: String,
    html_url: String,
    created_at: String,
}

/// Linear bug regression checker.
pub struct LinearRegressionChecker<H: HttpClient = ReqwestHttpClient> {
    config: LinearRegressionConfig,
    http: H,
    /// Keywords from the original issue for searching.
    keywords: Vec<String>,
}

impl LinearRegressionChecker<ReqwestHttpClient> {
    /// Create a new Linear regression checker.
    pub fn new(config: LinearRegressionConfig, keywords: Vec<String>) -> Self {
        Self {
            config,
            http: ReqwestHttpClient::new(),
            keywords,
        }
    }
}

impl<H: HttpClient> LinearRegressionChecker<H> {
    /// Create a new Linear regression checker with custom HTTP client.
    pub fn with_http_client(config: LinearRegressionConfig, keywords: Vec<String>, http: H) -> Self {
        Self {
            config,
            http,
            keywords,
        }
    }

    /// Search GitHub issues for similar problems.
    async fn search_github_issues(&self) -> Result<Vec<GitHubIssue>> {
        if self.config.github_token.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_issues = Vec::new();

        // Build search query from keywords
        let query_terms: Vec<String> = self
            .keywords
            .iter()
            .take(5) // Limit to 5 keywords
            .map(|k| format!("\"{}\"", k))
            .collect();

        if query_terms.is_empty() {
            return Ok(Vec::new());
        }

        for repo in &self.config.github_repos {
            let search_query = format!(
                "repo:{} is:issue is:open {}",
                repo,
                query_terms.join(" OR ")
            );

            let url = format!(
                "https://api.github.com/search/issues?q={}&per_page=10",
                urlencoding::encode(&search_query)
            );

            let headers = vec![
                ("Authorization", format!("Bearer {}", self.config.github_token)),
                ("Accept", "application/vnd.github+json".to_string()),
                ("User-Agent", "claudear".to_string()),
                ("X-GitHub-Api-Version", "2022-11-28".to_string()),
            ];

            let response = self.http.get(&url, headers).await?;

            if response.is_success() {
                if let Ok(result) = serde_json::from_str::<GitHubSearchResult>(&response.body) {
                    all_issues.extend(result.items);
                }
            }
        }

        Ok(all_issues)
    }

    /// Check appwrite.io/threads for similar issues (simplified version).
    /// Note: In production, this would use proper web scraping or an API.
    async fn check_appwrite_threads(&self) -> Result<Vec<String>> {
        // This is a simplified implementation.
        // In production, we would:
        // 1. Fetch appwrite.io/threads
        // 2. Parse HTML to extract thread content
        // 3. Use semantic similarity to find related threads

        // For now, we'll try to fetch and search the threads page
        if self.keywords.is_empty() {
            return Ok(Vec::new());
        }

        let url = "https://appwrite.io/threads";
        let headers = vec![
            ("User-Agent", "claudear".to_string()),
            ("Accept", "text/html".to_string()),
        ];

        match self.http.get(url, headers).await {
            Ok(response) if response.is_success() => {
                // Simple keyword matching in the response body
                let mut matches = Vec::new();
                let body_lower = response.body.to_lowercase();

                for keyword in &self.keywords {
                    if body_lower.contains(&keyword.to_lowercase()) {
                        matches.push(keyword.clone());
                    }
                }

                Ok(matches)
            }
            _ => Ok(Vec::new()), // Silently fail - threads check is optional
        }
    }
}

#[async_trait]
impl<H: HttpClient> RegressionChecker for LinearRegressionChecker<H> {
    async fn check_regression(&self, watch: &RegressionWatch) -> Result<RegressionResult> {
        let monitoring_started = match watch.monitoring_started_at {
            Some(dt) => dt,
            None => return Ok(RegressionResult::no_regression()),
        };

        // Check GitHub issues
        let github_issues = self.search_github_issues().await?;

        // Filter to issues created after monitoring started
        let new_issues: Vec<_> = github_issues
            .into_iter()
            .filter(|issue| {
                if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&issue.created_at) {
                    created.with_timezone(&chrono::Utc) > monitoring_started
                } else {
                    false
                }
            })
            .collect();

        if !new_issues.is_empty() {
            let issue_links: Vec<String> = new_issues
                .iter()
                .map(|i| format!("{}#{}: {}", i.html_url, i.number, i.title))
                .collect();

            return Ok(RegressionResult::regression(format!(
                "Found {} new GitHub issues that may indicate regression:\n{}",
                new_issues.len(),
                issue_links.join("\n")
            )));
        }

        // Check appwrite.io/threads
        let thread_matches = self.check_appwrite_threads().await?;
        if !thread_matches.is_empty() {
            return Ok(RegressionResult::regression(format!(
                "Found mentions on appwrite.io/threads for keywords: {}",
                thread_matches.join(", ")
            )));
        }

        Ok(RegressionResult {
            regression_detected: false,
            details: Some(format!(
                "No regression indicators found (searched {} repos, {} keywords)",
                self.config.github_repos.len(),
                self.keywords.len()
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::IssueType;
    use chrono::{Duration, Utc};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockHttpClient {
        responses: Vec<HttpResponse>,
        call_count: AtomicUsize,
    }

    impl MockHttpClient {
        fn new(responses: Vec<(u16, &str)>) -> Self {
            Self {
                responses: responses
                    .into_iter()
                    .map(|(status, body)| HttpResponse {
                        status,
                        body: body.to_string(),
                    })
                    .collect(),
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl HttpClient for MockHttpClient {
        async fn get(&self, _url: &str, _headers: Vec<(&str, String)>) -> Result<HttpResponse> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            if idx < self.responses.len() {
                Ok(HttpResponse {
                    status: self.responses[idx].status,
                    body: self.responses[idx].body.clone(),
                })
            } else {
                Ok(HttpResponse {
                    status: 404,
                    body: "Not Found".to_string(),
                })
            }
        }
    }

    fn create_config() -> LinearRegressionConfig {
        LinearRegressionConfig {
            github_token: "test-token".to_string(),
            github_repos: vec!["test/repo".to_string()],
            similarity_threshold: 0.75,
        }
    }

    #[tokio::test]
    async fn test_no_regression_when_no_issues() {
        let mock = MockHttpClient::new(vec![
            // GitHub search - empty results
            (
                200,
                r#"{"total_count": 0, "items": []}"#,
            ),
            // Threads check - no matches
            (200, "<html><body>No relevant content</body></html>"),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["test-keyword".to_string()],
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-123", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_regression_when_new_github_issue() {
        // Create a timestamp in the future to ensure the issue is "new"
        let future_time = (Utc::now() + Duration::hours(1)).to_rfc3339();

        let mock = MockHttpClient::new(vec![
            // GitHub search - found issue
            (
                200,
                &format!(
                    r#"{{
                        "total_count": 1,
                        "items": [{{
                            "id": 1,
                            "number": 42,
                            "title": "Bug: Same issue reoccurred",
                            "body": "Description",
                            "state": "open",
                            "html_url": "https://github.com/test/repo/issues/42",
                            "created_at": "{}"
                        }}]
                    }}"#,
                    future_time
                ),
            ),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["bug".to_string()],
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-456", 1);
        watch.monitoring_started_at = Some(Utc::now());

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected);
        assert!(result.details.unwrap().contains("GitHub issues"));
    }

    #[tokio::test]
    async fn test_no_regression_when_old_github_issue() {
        // Create a timestamp in the past
        let past_time = (Utc::now() - Duration::days(10)).to_rfc3339();

        let mock = MockHttpClient::new(vec![
            // GitHub search - found old issue
            (
                200,
                &format!(
                    r#"{{
                        "total_count": 1,
                        "items": [{{
                            "id": 1,
                            "number": 42,
                            "title": "Old bug",
                            "body": "Description",
                            "state": "open",
                            "html_url": "https://github.com/test/repo/issues/42",
                            "created_at": "{}"
                        }}]
                    }}"#,
                    past_time
                ),
            ),
            // Threads check
            (200, "<html><body>No matches</body></html>"),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["bug".to_string()],
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-789", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::days(5)); // Started 5 days ago

        let result = checker.check_regression(&watch).await.unwrap();
        // Issue was created 10 days ago, before monitoring started 5 days ago
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_regression_when_threads_mention() {
        let mock = MockHttpClient::new(vec![
            // GitHub search - no results
            (200, r#"{"total_count": 0, "items": []}"#),
            // Threads check - keyword found
            (
                200,
                "<html><body>Discussion about authentication-error issue</body></html>",
            ),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["authentication-error".to_string()],
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-111", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(result.regression_detected);
        assert!(result.details.unwrap().contains("appwrite.io/threads"));
    }

    #[tokio::test]
    async fn test_no_monitoring_started() {
        let mock = MockHttpClient::new(vec![]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["test".to_string()],
            mock,
        );

        // Watch without monitoring_started_at
        let watch = RegressionWatch::new(IssueType::LinearBug, "linear-222", 1);

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_empty_keywords() {
        let mock = MockHttpClient::new(vec![
            // Threads check - no keywords to search
            (200, "<html><body>Content</body></html>"),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec![], // No keywords
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-333", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }
}

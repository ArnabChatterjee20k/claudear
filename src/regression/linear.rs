//! Linear bug regression checker.
//!
//! Checks for regressions of Linear bugs by:
//! 1. Searching GitHub issues in related repositories
//! 2. Scraping appwrite.io/threads for similar mentions
//! 3. Using semantic similarity with embeddings to match issues

use crate::error::Result;
use crate::feedback::{cosine_similarity, EmbeddingClient};
use crate::github::{HttpClient, ReqwestHttpClient};
use crate::regression::{RegressionChecker, RegressionResult};
use crate::types::RegressionWatch;
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;

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
    #[allow(dead_code)]
    number: i64,
    title: String,
    body: Option<String>,
    #[allow(dead_code)]
    state: String,
    html_url: String,
    created_at: String,
}

/// Result of a similarity match.
#[derive(Debug, Clone)]
pub struct SimilarityMatch {
    /// The URL or identifier of the matched content.
    pub url: String,
    /// The title or summary of the matched content.
    pub title: String,
    /// The similarity score (0.0 to 1.0).
    pub similarity: f32,
}

/// Linear bug regression checker using semantic similarity.
pub struct LinearRegressionChecker<H: HttpClient = ReqwestHttpClient> {
    config: LinearRegressionConfig,
    http: H,
    /// Keywords from the original issue for searching (used for initial GitHub search).
    keywords: Vec<String>,
    /// The original issue text for semantic matching.
    /// Note: Stored for potential future use (e.g., logging, debugging).
    #[allow(dead_code)]
    original_issue_text: String,
    /// Pre-computed embedding for the original issue (if embedding client is available).
    original_embedding: Option<Vec<f32>>,
    /// Embedding client for semantic similarity (optional for testing).
    embedding_client: Option<Arc<EmbeddingClient>>,
}

impl LinearRegressionChecker<ReqwestHttpClient> {
    /// Create a new Linear regression checker.
    pub fn new(
        config: LinearRegressionConfig,
        keywords: Vec<String>,
        original_issue_text: String,
    ) -> Self {
        Self {
            config,
            http: ReqwestHttpClient::new(),
            keywords,
            original_issue_text,
            original_embedding: None,
            embedding_client: None,
        }
    }

    /// Create a new Linear regression checker with embedding support.
    pub async fn with_embeddings(
        config: LinearRegressionConfig,
        keywords: Vec<String>,
        original_issue_text: String,
        embedding_client: Arc<EmbeddingClient>,
    ) -> Result<Self> {
        // Pre-compute the embedding for the original issue
        let original_embedding = embedding_client.embed(&original_issue_text).await?;

        Ok(Self {
            config,
            http: ReqwestHttpClient::new(),
            keywords,
            original_issue_text,
            original_embedding: Some(original_embedding),
            embedding_client: Some(embedding_client),
        })
    }
}

impl<H: HttpClient> LinearRegressionChecker<H> {
    /// Create a new Linear regression checker with custom HTTP client (for testing).
    pub fn with_http_client(
        config: LinearRegressionConfig,
        keywords: Vec<String>,
        original_issue_text: String,
        http: H,
    ) -> Self {
        Self {
            config,
            http,
            keywords,
            original_issue_text,
            original_embedding: None,
            embedding_client: None,
        }
    }

    /// Create a new Linear regression checker with custom HTTP client and embedding support.
    pub fn with_http_client_and_embeddings(
        config: LinearRegressionConfig,
        keywords: Vec<String>,
        original_issue_text: String,
        original_embedding: Vec<f32>,
        embedding_client: Arc<EmbeddingClient>,
        http: H,
    ) -> Self {
        Self {
            config,
            http,
            keywords,
            original_issue_text,
            original_embedding: Some(original_embedding),
            embedding_client: Some(embedding_client),
        }
    }

    /// Check semantic similarity between the original issue and candidate text.
    async fn check_similarity(&self, candidate_text: &str) -> Result<f32> {
        match (&self.embedding_client, &self.original_embedding) {
            (Some(client), Some(original_emb)) => {
                let candidate_emb = client.embed(candidate_text).await?;
                Ok(cosine_similarity(original_emb, &candidate_emb))
            }
            _ => {
                // Fallback to keyword matching if no embedding client
                let candidate_lower = candidate_text.to_lowercase();
                let matches = self
                    .keywords
                    .iter()
                    .filter(|k| candidate_lower.contains(&k.to_lowercase()))
                    .count();
                // Return a similarity score based on keyword matches
                if self.keywords.is_empty() {
                    Ok(0.0)
                } else {
                    Ok(matches as f32 / self.keywords.len() as f32)
                }
            }
        }
    }

    /// Search GitHub issues for similar problems using semantic similarity.
    ///
    /// Returns issues that:
    /// 1. Match keywords (for initial filtering via GitHub search)
    /// 2. Pass semantic similarity threshold when compared to original issue
    ///
    /// Note: This method is available for standalone similarity searching.
    /// For regression checking with date filtering, use `search_github_issues_since`.
    #[allow(dead_code)]
    async fn search_github_issues(&self) -> Result<Vec<SimilarityMatch>> {
        if self.config.github_token.is_empty() {
            tracing::warn!("GitHub token not configured, skipping GitHub issue search for regression detection");
            return Ok(Vec::new());
        }

        let mut all_issues = Vec::new();

        // Build search query from keywords (for initial broad search)
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
                (
                    "Authorization",
                    format!("Bearer {}", self.config.github_token),
                ),
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

        // Now apply semantic similarity filtering
        let mut similar_issues = Vec::new();
        for issue in all_issues {
            // Combine title and body for semantic comparison
            let issue_text = format!("{}\n{}", issue.title, issue.body.as_deref().unwrap_or(""));

            let similarity = self.check_similarity(&issue_text).await?;

            if similarity >= self.config.similarity_threshold as f32 {
                similar_issues.push(SimilarityMatch {
                    url: issue.html_url.clone(),
                    title: issue.title.clone(),
                    similarity,
                });

                tracing::debug!(
                    "Found similar GitHub issue (similarity={:.2}): {} - {}",
                    similarity,
                    issue.html_url,
                    issue.title
                );
            }
        }

        Ok(similar_issues)
    }

    /// Search GitHub issues and filter by creation date and semantic similarity.
    async fn search_github_issues_since(
        &self,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<SimilarityMatch>> {
        if self.config.github_token.is_empty() {
            tracing::warn!("GitHub token not configured, skipping GitHub issue search for regression detection");
            return Ok(Vec::new());
        }

        let mut all_matches = Vec::new();

        // Build search query from keywords (for initial broad search)
        let query_terms: Vec<String> = self
            .keywords
            .iter()
            .take(5)
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
                (
                    "Authorization",
                    format!("Bearer {}", self.config.github_token),
                ),
                ("Accept", "application/vnd.github+json".to_string()),
                ("User-Agent", "claudear".to_string()),
                ("X-GitHub-Api-Version", "2022-11-28".to_string()),
            ];

            let response = self.http.get(&url, headers).await?;

            if response.is_success() {
                if let Ok(result) = serde_json::from_str::<GitHubSearchResult>(&response.body) {
                    // Filter by date first
                    let new_issues: Vec<_> = result
                        .items
                        .into_iter()
                        .filter(|issue| {
                            if let Ok(created) =
                                chrono::DateTime::parse_from_rfc3339(&issue.created_at)
                            {
                                created.with_timezone(&chrono::Utc) > since
                            } else {
                                false
                            }
                        })
                        .collect();

                    // Then apply semantic similarity
                    for issue in new_issues {
                        let issue_text =
                            format!("{}\n{}", issue.title, issue.body.as_deref().unwrap_or(""));

                        let similarity = self.check_similarity(&issue_text).await?;

                        if similarity >= self.config.similarity_threshold as f32 {
                            all_matches.push(SimilarityMatch {
                                url: issue.html_url.clone(),
                                title: issue.title.clone(),
                                similarity,
                            });

                            tracing::info!(
                                "Found semantically similar GitHub issue (similarity={:.2}): {} - {}",
                                similarity,
                                issue.html_url,
                                issue.title
                            );
                        }
                    }
                }
            }
        }

        Ok(all_matches)
    }

    /// Check appwrite.io/threads for similar issues using semantic similarity.
    ///
    /// This extracts text content from the threads page and uses embedding-based
    /// similarity to find semantically related discussions.
    async fn check_appwrite_threads(&self) -> Result<Vec<SimilarityMatch>> {
        let url = "https://appwrite.io/threads";
        let headers = vec![
            ("User-Agent", "claudear".to_string()),
            ("Accept", "text/html".to_string()),
        ];

        match self.http.get(url, headers).await {
            Ok(response) if response.is_success() => {
                // Extract thread content sections from HTML
                // This is a simplified extraction - in production, use a proper HTML parser
                let thread_sections = self.extract_thread_sections(&response.body);

                let mut matches = Vec::new();

                for (title, content) in thread_sections {
                    let thread_text = format!("{}\n{}", title, content);
                    let similarity = self.check_similarity(&thread_text).await?;

                    if similarity >= self.config.similarity_threshold as f32 {
                        matches.push(SimilarityMatch {
                            url: format!(
                                "https://appwrite.io/threads#{}",
                                title.to_lowercase().replace(' ', "-")
                            ),
                            title: title.clone(),
                            similarity,
                        });

                        tracing::info!(
                            "Found semantically similar thread (similarity={:.2}): {}",
                            similarity,
                            title
                        );
                    }
                }

                Ok(matches)
            }
            Ok(response) => {
                // Non-success HTTP status - log warning but continue
                tracing::warn!(
                    url = %url,
                    status = response.status,
                    "Failed to fetch appwrite.io/threads, skipping threads check"
                );
                Ok(Vec::new())
            }
            Err(e) => {
                // Network or other error - log warning but continue
                tracing::warn!(
                    url = %url,
                    error = %e,
                    "Error fetching appwrite.io/threads, skipping threads check"
                );
                Ok(Vec::new())
            }
        }
    }

    /// Extract thread sections from HTML content.
    ///
    /// This is a simplified implementation that looks for common HTML patterns.
    /// In production, use a proper HTML parser like scraper or select.
    fn extract_thread_sections(&self, html: &str) -> Vec<(String, String)> {
        let mut sections = Vec::new();

        // Simple extraction: look for <article>, <div class="thread">, or similar patterns
        // Use case-insensitive search on the original string to avoid UTF-8 byte offset issues
        let title_patterns = [
            "<h2>",
            "<h3>",
            "<H2>",
            "<H3>",
            "<article",
            "<Article",
            "<ARTICLE",
            "<div class=\"thread",
            "<div class=\"post",
            "<DIV class=\"thread",
        ];

        for pattern in title_patterns {
            if let Some(start) = html.find(pattern) {
                // Safely calculate the end position respecting UTF-8 boundaries
                // Take up to 2000 bytes but ensure we end on a valid char boundary
                let mut end = (start + 2000).min(html.len());
                while end > start && !html.is_char_boundary(end) {
                    end -= 1;
                }

                if end <= start {
                    continue;
                }

                let section = &html[start..end];

                // Try to extract title (text between tags)
                if let Some(title_end) = section.find("</") {
                    let title_content = &section[..title_end];
                    // Remove HTML tags from title
                    let title = self.strip_html_tags(title_content);

                    // Content is the rest
                    let content = self.strip_html_tags(&section[title_end..]);

                    if !title.is_empty() && !content.is_empty() {
                        sections.push((title, content));
                    }
                }
            }
        }

        // If no structured content found, treat the whole page as one section
        if sections.is_empty() {
            let full_text = self.strip_html_tags(html);
            if !full_text.is_empty() {
                sections.push(("Appwrite Threads Page".to_string(), full_text));
            }
        }

        sections
    }

    /// Strip HTML tags from text content.
    fn strip_html_tags(&self, html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;

        for c in html.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(c),
                _ => {}
            }
        }

        // Normalize whitespace
        result
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    }
}

#[async_trait]
impl<H: HttpClient> RegressionChecker for LinearRegressionChecker<H> {
    async fn check_regression(&self, watch: &RegressionWatch) -> Result<RegressionResult> {
        let monitoring_started = match watch.monitoring_started_at {
            Some(dt) => dt,
            None => return Ok(RegressionResult::no_regression()),
        };

        // Check GitHub issues using semantic similarity
        let similar_issues = self.search_github_issues_since(monitoring_started).await?;

        if !similar_issues.is_empty() {
            let issue_links: Vec<String> = similar_issues
                .iter()
                .map(|m| {
                    format!(
                        "{} (similarity: {:.0}%) - {}",
                        m.url,
                        m.similarity * 100.0,
                        m.title
                    )
                })
                .collect();

            return Ok(RegressionResult::regression(format!(
                "Found {} semantically similar GitHub issues that may indicate regression:\n{}",
                similar_issues.len(),
                issue_links.join("\n")
            )));
        }

        // Check appwrite.io/threads using semantic similarity
        let thread_matches = self.check_appwrite_threads().await?;
        if !thread_matches.is_empty() {
            let thread_links: Vec<String> = thread_matches
                .iter()
                .map(|m| {
                    format!(
                        "{} (similarity: {:.0}%) - {}",
                        m.url,
                        m.similarity * 100.0,
                        m.title
                    )
                })
                .collect();

            return Ok(RegressionResult::regression(format!(
                "Found {} semantically similar discussions on appwrite.io/threads:\n{}",
                thread_matches.len(),
                thread_links.join("\n")
            )));
        }

        let method = if self.embedding_client.is_some() {
            "semantic similarity (embeddings)"
        } else {
            "keyword matching (fallback)"
        };

        Ok(RegressionResult {
            regression_detected: false,
            details: Some(format!(
                "No regression indicators found using {} (searched {} repos, threshold: {:.0}%)",
                method,
                self.config.github_repos.len(),
                self.config.similarity_threshold * 100.0
            )),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::HttpResponse;
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
            (200, r#"{"total_count": 0, "items": []}"#),
            // Threads check - no matches
            (200, "<html><body>No relevant content</body></html>"),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["test-keyword".to_string()],
            "Original issue about test-keyword problem".to_string(),
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

        // Use lower similarity threshold for keyword-only fallback matching
        let mut config = create_config();
        config.similarity_threshold = 0.5; // Lower threshold for keyword matching

        let mock = MockHttpClient::new(vec![
            // GitHub search - found issue with same keyword "bug"
            (
                200,
                &format!(
                    r#"{{
                        "total_count": 1,
                        "items": [{{
                            "id": 1,
                            "number": 42,
                            "title": "Bug: Same issue reoccurred",
                            "body": "This bug is causing problems",
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
            config,
            vec!["bug".to_string()],
            "Bug: Original issue about a bug problem".to_string(),
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
            "Bug: Original issue with bug keyword".to_string(),
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
        // Use lower similarity threshold for keyword-only fallback matching
        let mut config = create_config();
        config.similarity_threshold = 0.5; // Lower threshold for keyword matching

        let mock = MockHttpClient::new(vec![
            // GitHub search - no results
            (200, r#"{"total_count": 0, "items": []}"#),
            // Threads check - keyword found in content
            (
                200,
                "<html><body><h2>Authentication Error Discussion</h2>Discussion about authentication-error issue and how to solve it</body></html>",
            ),
        ]);

        let checker = LinearRegressionChecker::with_http_client(
            config,
            vec!["authentication-error".to_string()],
            "Authentication-error: Users cannot log in due to authentication-error".to_string(),
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
            "Test issue with test keyword".to_string(),
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
            "Original issue without specific keywords".to_string(),
            mock,
        );

        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-333", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        let result = checker.check_regression(&watch).await.unwrap();
        assert!(!result.regression_detected);
    }

    #[tokio::test]
    async fn test_similarity_match_struct() {
        let match_result = SimilarityMatch {
            url: "https://example.com/issue/1".to_string(),
            title: "Test Issue".to_string(),
            similarity: 0.85,
        };

        assert_eq!(match_result.url, "https://example.com/issue/1");
        assert_eq!(match_result.title, "Test Issue");
        assert!((match_result.similarity - 0.85).abs() < 0.001);
    }

    #[tokio::test]
    async fn test_strip_html_tags() {
        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["test".to_string()],
            "Test issue".to_string(),
            MockHttpClient::new(vec![]),
        );

        let html = "<p>Hello <strong>world</strong>!</p>";
        let stripped = checker.strip_html_tags(html);
        assert_eq!(stripped, "Hello world!");
    }

    #[tokio::test]
    async fn test_extract_thread_sections_with_headers() {
        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["test".to_string()],
            "Test issue".to_string(),
            MockHttpClient::new(vec![]),
        );

        let html =
            "<html><body><h2>Thread Title</h2>This is the content of the thread</body></html>";
        let sections = checker.extract_thread_sections(html);

        assert!(!sections.is_empty());
    }

    #[tokio::test]
    async fn test_fallback_keyword_similarity() {
        // Without embeddings, the checker should fall back to keyword matching
        let checker = LinearRegressionChecker::with_http_client(
            create_config(),
            vec!["authentication".to_string(), "error".to_string()],
            "Authentication error when logging in".to_string(),
            MockHttpClient::new(vec![]),
        );

        // Test the check_similarity fallback (keyword matching)
        // This is testing the internal method indirectly through the behavior
        let mut watch = RegressionWatch::new(IssueType::LinearBug, "linear-test", 1);
        watch.monitoring_started_at = Some(Utc::now() - Duration::hours(1));

        // Should not error even without embeddings
        let result = checker.check_regression(&watch).await;
        assert!(result.is_ok());
    }
}

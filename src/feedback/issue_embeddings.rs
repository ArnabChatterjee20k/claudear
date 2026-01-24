//! Issue embedding service for semantic similarity search.
//!
//! Provides functionality to embed issues and find similar past issues
//! to improve Claude's context when processing new issues.

use crate::error::Result;
use crate::feedback::EmbeddingClient;
use crate::storage::{FixAttemptTracker, SqliteTracker};
use crate::types::{Issue, IssueEmbedding, SimilarIssue};
use chrono::Utc;
use std::sync::Arc;

/// Configuration for issue embedding.
#[derive(Debug, Clone)]
pub struct IssueEmbeddingConfig {
    /// Whether embeddings are enabled.
    pub enabled: bool,
    /// Minimum similarity score to consider a match (0.0-1.0).
    pub min_similarity: f64,
    /// Maximum number of similar issues to return.
    pub max_similar_issues: usize,
}

impl Default for IssueEmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_similarity: 0.7,
            max_similar_issues: 5,
        }
    }
}

/// A similar issue with its details and similarity score.
#[derive(Debug, Clone)]
pub struct SimilarIssueWithDetails {
    /// The original issue embedding.
    pub embedding: IssueEmbedding,
    /// Similarity score (0.0-1.0).
    pub similarity: f64,
    /// Outcome of the fix attempt (if known).
    pub outcome: Option<String>,
    /// PR URL (if created).
    pub pr_url: Option<String>,
}

/// Service for managing issue embeddings and finding similar issues.
pub struct IssueEmbeddingService {
    embedding_client: Arc<EmbeddingClient>,
    tracker: Arc<SqliteTracker>,
    config: IssueEmbeddingConfig,
}

impl IssueEmbeddingService {
    /// Create a new issue embedding service.
    pub fn new(
        embedding_client: Arc<EmbeddingClient>,
        tracker: Arc<SqliteTracker>,
        config: IssueEmbeddingConfig,
    ) -> Self {
        Self {
            embedding_client,
            tracker,
            config,
        }
    }

    /// Create with default configuration.
    pub fn with_defaults(
        embedding_client: Arc<EmbeddingClient>,
        tracker: Arc<SqliteTracker>,
    ) -> Self {
        Self::new(embedding_client, tracker, IssueEmbeddingConfig::default())
    }

    /// Check if embedding service is enabled.
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Embed a single issue and store it in the database.
    pub async fn embed_issue(&self, issue: &Issue, source: &str) -> Result<IssueEmbedding> {
        // Build text to embed from issue content
        let text = build_embedding_text(issue);

        // Generate embedding
        let embedding_vec = self.embedding_client.embed(&text).await?;

        // Create embedding record
        let mut embedding = IssueEmbedding::new(source, &issue.id, embedding_vec);
        embedding.short_id = Some(issue.short_id.clone());
        embedding.title = Some(issue.title.clone());
        embedding.embedding_model = Some(self.embedding_client.model().to_string());
        embedding.created_at = Utc::now();

        // Store in database
        self.tracker.store_embedding(&embedding)?;

        tracing::debug!(
            source = source,
            issue_id = %issue.id,
            short_id = %issue.short_id,
            "Stored issue embedding"
        );

        Ok(embedding)
    }

    /// Embed multiple issues efficiently.
    pub async fn embed_batch(&self, issues: &[Issue], source: &str) -> Result<Vec<IssueEmbedding>> {
        if issues.is_empty() {
            return Ok(Vec::new());
        }

        // Build texts for all issues
        let texts: Vec<String> = issues.iter().map(build_embedding_text).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        // Generate embeddings in batch
        let embeddings_vecs = self.embedding_client.embed_batch(&text_refs).await?;

        // Create and store embedding records
        let mut results = Vec::with_capacity(issues.len());
        for (issue, embedding_vec) in issues.iter().zip(embeddings_vecs) {
            let mut embedding = IssueEmbedding::new(source, &issue.id, embedding_vec);
            embedding.short_id = Some(issue.short_id.clone());
            embedding.title = Some(issue.title.clone());
            embedding.embedding_model = Some(self.embedding_client.model().to_string());
            embedding.created_at = Utc::now();

            self.tracker.store_embedding(&embedding)?;
            results.push(embedding);
        }

        tracing::info!(
            source = source,
            count = results.len(),
            "Stored batch of issue embeddings"
        );

        Ok(results)
    }

    /// Find similar issues for a given issue.
    ///
    /// Returns issues sorted by similarity score (highest first).
    pub async fn find_similar(&self, issue: &Issue, source: &str) -> Result<Vec<SimilarIssueWithDetails>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        // Generate embedding for the query issue
        let text = build_embedding_text(issue);
        let query_embedding = self.embedding_client.embed(&text).await?;

        // Get all embeddings from the database for this source
        let stored_embeddings = self.tracker.get_all_embeddings(Some(source))?;

        if stored_embeddings.is_empty() {
            return Ok(Vec::new());
        }

        // Calculate similarities
        let mut similarities: Vec<(IssueEmbedding, f64)> = stored_embeddings
            .into_iter()
            .filter(|e| e.issue_id != issue.id) // Exclude the query issue itself
            .map(|e| {
                let sim = crate::feedback::cosine_similarity(&query_embedding, &e.embedding);
                (e, sim as f64)
            })
            .filter(|(_, sim)| *sim >= self.config.min_similarity)
            .collect();

        // Sort by similarity (descending)
        similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top N
        let top_similar: Vec<(IssueEmbedding, f64)> = similarities
            .into_iter()
            .take(self.config.max_similar_issues)
            .collect();

        // Enrich with fix attempt details
        let mut results = Vec::with_capacity(top_similar.len());
        for (embedding, similarity) in top_similar {
            // Look up fix attempt for this issue
            let attempt = self.tracker.get_attempt(&embedding.source, &embedding.issue_id).ok().flatten();

            let (outcome, pr_url) = match attempt {
                Some(a) => (Some(a.status.to_string()), a.pr_url),
                None => (None, None),
            };

            results.push(SimilarIssueWithDetails {
                embedding,
                similarity,
                outcome,
                pr_url,
            });
        }

        // Store similar issue relationships
        for result in &results {
            let similar = SimilarIssue {
                id: 0,
                source_issue_id: issue.id.clone(),
                similar_issue_id: result.embedding.issue_id.clone(),
                similarity_score: result.similarity,
                computed_at: Utc::now(),
            };
            if let Err(e) = self.tracker.store_similar_issue(&similar) {
                tracing::warn!(error = %e, "Failed to store similar issue relationship");
            }
        }

        tracing::debug!(
            source = source,
            issue_id = %issue.id,
            similar_count = results.len(),
            "Found similar issues"
        );

        Ok(results)
    }

    /// Get an existing embedding for an issue.
    pub fn get_embedding(&self, source: &str, issue_id: &str) -> Result<Option<IssueEmbedding>> {
        self.tracker.get_embedding(source, issue_id)
    }

    /// Check if an issue already has an embedding.
    pub fn has_embedding(&self, source: &str, issue_id: &str) -> bool {
        self.get_embedding(source, issue_id).map(|e| e.is_some()).unwrap_or(false)
    }
}

/// Build text content for embedding from an issue.
fn build_embedding_text(issue: &Issue) -> String {
    let mut parts = Vec::new();

    // Title is most important
    parts.push(issue.title.clone());

    // Description if available
    if let Some(ref desc) = issue.description {
        parts.push(desc.clone());
    }

    // Add labels from metadata if available
    if let Some(labels) = issue.metadata.get("labels") {
        if let Some(labels_arr) = labels.as_array() {
            let label_strs: Vec<&str> = labels_arr
                .iter()
                .filter_map(|l| l.as_str())
                .collect();
            if !label_strs.is_empty() {
                parts.push(format!("Labels: {}", label_strs.join(", ")));
            }
        }
    }

    // Stack trace from metadata if available (very important for bug similarity)
    if let Some(stack) = issue.metadata.get("stack_trace").and_then(|v| v.as_str()) {
        // Truncate very long stack traces
        let truncated = if stack.len() > 2000 {
            format!("{}...", &stack[..2000])
        } else {
            stack.to_string()
        };
        parts.push(truncated);
    }

    // Also check for error message in metadata
    if let Some(error) = issue.metadata.get("error_message").and_then(|v| v.as_str()) {
        parts.push(error.to_string());
    }

    parts.join("\n\n")
}

/// Format similar issues as context for Claude.
pub fn format_similar_issues_context(similar: &[SimilarIssueWithDetails]) -> String {
    if similar.is_empty() {
        return String::new();
    }

    let mut context = String::from("\n\n## Similar Past Issues\n\n");
    context.push_str("The following similar issues have been processed before. ");
    context.push_str("Use this context to inform your approach:\n\n");

    for (i, sim) in similar.iter().enumerate() {
        context.push_str(&format!(
            "### {}. {} (Similarity: {:.0}%)\n",
            i + 1,
            sim.embedding.short_id.as_deref().unwrap_or(&sim.embedding.issue_id),
            sim.similarity * 100.0
        ));

        if let Some(ref title) = sim.embedding.title {
            context.push_str(&format!("**Title:** {}\n", title));
        }

        if let Some(ref outcome) = sim.outcome {
            context.push_str(&format!("**Outcome:** {}\n", outcome));
        }

        if let Some(ref pr_url) = sim.pr_url {
            context.push_str(&format!("**PR:** {}\n", pr_url));
        }

        context.push('\n');
    }

    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{IssuePriority, IssueStatus};
    use std::collections::HashMap;

    #[test]
    fn test_build_embedding_text() {
        let mut metadata = HashMap::new();
        metadata.insert(
            "labels".to_string(),
            serde_json::json!(["bug", "auth"]),
        );
        metadata.insert(
            "stack_trace".to_string(),
            serde_json::json!("Error at auth.rs:42"),
        );

        let issue = Issue {
            id: "123".to_string(),
            short_id: "PROJ-123".to_string(),
            title: "Fix the login bug".to_string(),
            description: Some("Users can't log in".to_string()),
            url: "https://example.com/issue/123".to_string(),
            source: "linear".to_string(),
            priority: IssuePriority::High,
            status: IssueStatus::Open,
            metadata,
            created_at: None,
            updated_at: None,
        };

        let text = build_embedding_text(&issue);
        assert!(text.contains("Fix the login bug"));
        assert!(text.contains("Users can't log in"));
        assert!(text.contains("Labels: bug, auth"));
        assert!(text.contains("Error at auth.rs:42"));
    }

    #[test]
    fn test_format_similar_issues_context() {
        let similar = vec![SimilarIssueWithDetails {
            embedding: IssueEmbedding::new("linear", "456", vec![0.1, 0.2]),
            similarity: 0.85,
            outcome: Some("merged".to_string()),
            pr_url: Some("https://github.com/org/repo/pull/42".to_string()),
        }];

        let context = format_similar_issues_context(&similar);
        assert!(context.contains("Similar Past Issues"));
        assert!(context.contains("85%"));
        assert!(context.contains("merged"));
        assert!(context.contains("pull/42"));
    }
}

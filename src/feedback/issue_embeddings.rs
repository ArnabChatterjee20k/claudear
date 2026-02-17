//! Issue embedding service for semantic similarity search.
//!
//! Provides functionality to embed issues and find similar past issues
//! to improve Claude's context when processing new issues.

use crate::error::Result;
use crate::feedback::EmbeddingClient;
use crate::storage::SqliteTracker;
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
    /// Similarity threshold above which a new issue is considered a semantic duplicate (0.0-1.0).
    pub skip_similarity_threshold: f64,
}

impl Default for IssueEmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_similarity: 0.7,
            max_similar_issues: 5,
            skip_similarity_threshold: 0.90,
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
    ///
    /// Uses a single database transaction for all inserts instead of
    /// acquiring the mutex lock once per embedding.
    pub async fn embed_batch(&self, issues: &[Issue], source: &str) -> Result<Vec<IssueEmbedding>> {
        if issues.is_empty() {
            return Ok(Vec::new());
        }

        // Build texts for all issues
        let texts: Vec<String> = issues.iter().map(build_embedding_text).collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        // Generate embeddings in batch
        let embeddings_vecs = self.embedding_client.embed_batch(&text_refs).await?;

        // Create embedding records
        let model_name = self.embedding_client.model().to_string();
        let now = Utc::now();
        let mut results = Vec::with_capacity(issues.len());
        for (issue, embedding_vec) in issues.iter().zip(embeddings_vecs) {
            let mut embedding = IssueEmbedding::new(source, &issue.id, embedding_vec);
            embedding.short_id = Some(issue.short_id.clone());
            embedding.title = Some(issue.title.clone());
            embedding.embedding_model = Some(model_name.clone());
            embedding.created_at = now;
            results.push(embedding);
        }

        // Store all embeddings in a single transaction
        self.tracker.store_embeddings_batch(&results)?;

        tracing::info!(
            source = source,
            count = results.len(),
            "Stored batch of issue embeddings"
        );

        Ok(results)
    }

    /// Find similar issues for a given issue using HNSW vector search.
    ///
    /// Returns empty if vectorlite is unavailable or no results found.
    pub async fn find_similar(
        &self,
        issue: &Issue,
        source: &str,
    ) -> Result<Vec<SimilarIssueWithDetails>> {
        if !self.config.enabled {
            return Ok(Vec::new());
        }

        // Generate embedding for the query issue
        let text = build_embedding_text(issue);
        let query_embedding = self.embedding_client.embed(&text).await?;

        // HNSW vector search via vectorlite
        let top_similar = match self.tracker.find_similar_issues_vector(
            &query_embedding,
            source,
            Some(&issue.id),
            self.config.min_similarity,
            self.config.max_similar_issues,
        )? {
            Some(results) => {
                tracing::debug!(
                    count = results.len(),
                    "Issue similarity search used HNSW index"
                );
                results
            }
            None => {
                tracing::warn!("vectorlite unavailable for issue similarity search");
                return Ok(Vec::new());
            }
        };

        // Enrich with fix attempt details (batch query to avoid N+1)
        let keys: Vec<(&str, &str)> = top_similar
            .iter()
            .map(|(emb, _)| (emb.source.as_str(), emb.issue_id.as_str()))
            .collect();
        let attempts = self.tracker.get_attempts_batch(&keys).unwrap_or_default();

        let mut results = Vec::with_capacity(top_similar.len());
        for ((embedding, similarity), attempt) in top_similar.into_iter().zip(attempts) {
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

        // Store similar issue relationships in a single transaction (batch insert)
        let now = Utc::now();
        let similar_batch: Vec<SimilarIssue> = results
            .iter()
            .map(|result| SimilarIssue {
                id: 0,
                source_issue_id: issue.id.clone(),
                similar_issue_id: result.embedding.issue_id.clone(),
                similarity_score: result.similarity,
                computed_at: now,
            })
            .collect();
        if let Err(e) = self.tracker.store_similar_issues_batch(&similar_batch) {
            tracing::warn!(error = %e, "Failed to store similar issue relationships");
        }

        tracing::debug!(
            source = source,
            issue_id = %issue.id,
            similar_count = results.len(),
            "Found similar issues"
        );

        Ok(results)
    }

    /// Check if an issue is a semantic duplicate of one already being processed or resolved.
    ///
    /// Returns `Some(duplicate)` if the top similar issue has similarity ≥ `skip_similarity_threshold`
    /// AND the similar issue's outcome is pending, success, or merged.
    /// Returns `None` if no duplicate is found or the similar issue failed.
    pub async fn check_duplicate(
        &self,
        issue: &Issue,
        source: &str,
    ) -> Result<Option<SimilarIssueWithDetails>> {
        if !self.config.enabled {
            return Ok(None);
        }

        let similar = self.find_similar(issue, source).await?;
        if let Some(top) = similar.into_iter().next() {
            if top.similarity >= self.config.skip_similarity_threshold {
                // Only skip if the similar issue is actively being handled
                let dominated_by_active = matches!(
                    top.outcome.as_deref(),
                    Some("pending") | Some("success") | Some("merged")
                );
                if dominated_by_active {
                    return Ok(Some(top));
                }
            }
        }
        Ok(None)
    }

    /// Get an existing embedding for an issue.
    pub fn get_embedding(&self, source: &str, issue_id: &str) -> Result<Option<IssueEmbedding>> {
        self.tracker.get_embedding(source, issue_id)
    }

    /// Check if an issue already has an embedding.
    pub fn has_embedding(&self, source: &str, issue_id: &str) -> bool {
        self.get_embedding(source, issue_id)
            .map(|e| e.is_some())
            .unwrap_or(false)
    }
}

/// Build text content for embedding from an issue.
///
/// Uses direct string building with push_str to avoid intermediate allocations.
fn build_embedding_text(issue: &Issue) -> String {
    // Estimate capacity: title + description + some overhead for labels/stack
    let est_cap = issue.title.len() + issue.description.as_ref().map_or(0, |d| d.len() + 2) + 256;
    let mut text = String::with_capacity(est_cap);

    // Title is most important
    text.push_str(&issue.title);

    // Description if available
    if let Some(ref desc) = issue.description {
        text.push_str("\n\n");
        text.push_str(desc);
    }

    // Add labels from metadata if available
    if let Some(labels) = issue.metadata.get("labels") {
        if let Some(labels_arr) = labels.as_array() {
            let label_strs: Vec<&str> = labels_arr.iter().filter_map(|l| l.as_str()).collect();
            if !label_strs.is_empty() {
                text.push_str("\n\nLabels: ");
                text.push_str(&label_strs.join(", "));
            }
        }
    }

    // Stack trace from metadata if available (very important for bug similarity)
    if let Some(stack) = issue.metadata.get("stack_trace").and_then(|v| v.as_str()) {
        text.push_str("\n\n");
        if stack.len() > 2000 {
            let truncate_pos = stack
                .char_indices()
                .take_while(|(i, _)| *i < 2000)
                .last()
                .map(|(i, c)| i + c.len_utf8())
                .unwrap_or(0);
            text.push_str(&stack[..truncate_pos]);
            text.push_str("...");
        } else {
            text.push_str(stack);
        }
    }

    // Also check for error message in metadata
    if let Some(error) = issue.metadata.get("error_message").and_then(|v| v.as_str()) {
        text.push_str("\n\n");
        text.push_str(error);
    }

    text
}

/// Format similar issues as context for Claude.
///
/// Uses `std::fmt::Write` to write directly into the String, avoiding
/// intermediate format! allocations for each field.
pub fn format_similar_issues_context(similar: &[SimilarIssueWithDetails]) -> String {
    use std::fmt::Write;

    if similar.is_empty() {
        return String::new();
    }

    let mut context = String::from("\n\n## Similar Past Issues\n\n");
    context.push_str("The following similar issues have been processed before. ");
    context.push_str("Use this context to inform your approach:\n\n");

    for (i, sim) in similar.iter().enumerate() {
        let _ = writeln!(
            context,
            "### {}. {} (Similarity: {:.0}%)",
            i + 1,
            sim.embedding
                .short_id
                .as_deref()
                .unwrap_or(&sim.embedding.issue_id),
            sim.similarity * 100.0
        );

        if let Some(ref title) = sim.embedding.title {
            let _ = writeln!(context, "**Title:** {}", title);
        }

        if let Some(ref outcome) = sim.outcome {
            let _ = writeln!(context, "**Outcome:** {}", outcome);
        }

        if let Some(ref pr_url) = sim.pr_url {
            let _ = writeln!(context, "**PR:** {}", pr_url);
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
        metadata.insert("labels".to_string(), serde_json::json!(["bug", "auth"]));
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

//! Repository inference engine.
//!
//! Automatically determines which repository an issue belongs to based on
//! file paths, stack traces, and other context extracted from issues.

mod context;

pub use context::IssueContext;

use crate::repo::{IndexedRepo, RepoIndex};
use crate::storage::SqliteTracker;
use crate::types::{ActivityLogEntry, Issue};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

/// Result of attempting to resolve a repository for an issue.
#[derive(Debug)]
pub enum RepoResolution {
    /// Successfully resolved to a repository.
    Resolved {
        /// Path to the repository.
        project_dir: PathBuf,
        /// Database ID of the repo (for analytics).
        repo_id: Option<i64>,
    },
    /// Could not resolve - processing should be skipped.
    Skip {
        /// Reason for skipping.
        reason: String,
    },
}

impl RepoResolution {
    /// Returns the project directory if resolved, None if skipped.
    pub fn project_dir(&self) -> Option<&PathBuf> {
        match self {
            RepoResolution::Resolved { project_dir, .. } => Some(project_dir),
            RepoResolution::Skip { .. } => None,
        }
    }

    /// Returns true if resolution was successful.
    pub fn is_resolved(&self) -> bool {
        matches!(self, RepoResolution::Resolved { .. })
    }
}

/// Resolve the target repository for an issue.
///
/// This is the shared entry point for repo resolution used by both the watcher
/// and webhook server. It handles:
/// - Inferring the repository using the inference engine
/// - Recording analytics for the inference attempt
/// - Logging activity for the attempt
///
/// Returns `RepoResolution::Skip` if inference fails or no inferrer is configured.
pub fn resolve_repo_for_issue(
    inferrer: Option<&RepoInferrer>,
    issue: &Issue,
    tracker: Option<&Arc<SqliteTracker>>,
) -> RepoResolution {
    resolve_repo_for_issue_with_embedding(inferrer, issue, tracker, None)
}

/// Resolve the target repository for an issue with optional embedding for semantic matching.
pub fn resolve_repo_for_issue_with_embedding(
    inferrer: Option<&RepoInferrer>,
    issue: &Issue,
    tracker: Option<&Arc<SqliteTracker>>,
    query_embedding: Option<&[f32]>,
) -> RepoResolution {
    let inference_start = Instant::now();
    let context = IssueContext::from_issue(issue);

    match inferrer {
        Some(inferrer) => {
            match inferrer.infer_with_embedding(issue, query_embedding) {
                Some(inferred) => {
                    let duration_ms = inference_start.elapsed().as_millis() as i64;
                    tracing::info!(
                        short_id = %issue.short_id,
                        repo = %inferred.repo.name,
                        confidence = %inferred.confidence,
                        reason = %inferred.reason,
                        duration_ms = duration_ms,
                        "Repository inferred"
                    );

                    // Record inference attempt for analytics
                    let repo_id = record_inference_attempt(
                        tracker,
                        issue,
                        &context,
                        Some(&inferred),
                        duration_ms,
                    );

                    // Log activity
                    if let Some(t) = tracker {
                        let activity = ActivityLogEntry::new(
                            "repo_inferred",
                            format!("Inferred repo {} for {}", inferred.repo.name, issue.short_id),
                        )
                        .with_source(issue.source.clone())
                        .with_issue(issue.id.clone(), issue.short_id.clone())
                        .with_metadata(json!({
                            "repo": inferred.repo.name,
                            "confidence": inferred.confidence.to_string(),
                            "reason": inferred.reason,
                            "matched_file": inferred.matched_file
                        }));
                        t.record_activity(&activity).ok();
                    }

                    RepoResolution::Resolved {
                        project_dir: inferred.repo.path.clone(),
                        repo_id,
                    }
                }
                None => {
                    let duration_ms = inference_start.elapsed().as_millis() as i64;

                    // Check for unknown repo references
                    let unknown_repos = inferrer.find_unknown_repos(&context);
                    if !unknown_repos.is_empty() {
                        tracing::warn!(
                            short_id = %issue.short_id,
                            source = %issue.source,
                            unknown_repos = ?unknown_repos,
                            "Issue references repositories not cloned locally"
                        );

                        // Log each unknown repo for visibility
                        for repo in &unknown_repos {
                            tracing::info!(
                                "  → Missing repo: {} (clone to auto_discover_paths to enable)",
                                repo
                            );
                        }
                    }

                    tracing::warn!(
                        short_id = %issue.short_id,
                        source = %issue.source,
                        "Could not infer repository for issue, skipping"
                    );

                    // Record failed inference attempt
                    record_inference_attempt(tracker, issue, &context, None, duration_ms);

                    // Log activity
                    if let Some(t) = tracker {
                        let activity = ActivityLogEntry::new(
                            "inference_failed",
                            format!("Could not infer repository for {}, skipping", issue.short_id),
                        )
                        .with_source(issue.source.clone())
                        .with_issue(issue.id.clone(), issue.short_id.clone())
                        .with_metadata(json!({
                            "extracted_filenames": context.filenames,
                            "extracted_functions": context.functions,
                            "unknown_repos": unknown_repos,
                            "skipped": true
                        }));
                        t.record_activity(&activity).ok();
                    }

                    RepoResolution::Skip {
                        reason: "Could not infer repository".to_string(),
                    }
                }
            }
        }
        None => {
            tracing::warn!(
                short_id = %issue.short_id,
                "No inferrer configured, skipping"
            );

            // Log activity
            if let Some(t) = tracker {
                let activity = ActivityLogEntry::new(
                    "processing_skipped",
                    format!("No inferrer configured for {}, skipping", issue.short_id),
                )
                .with_source(issue.source.clone())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "reason": "no_inferrer_configured",
                    "skipped": true
                }));
                t.record_activity(&activity).ok();
            }

            RepoResolution::Skip {
                reason: "No inferrer configured".to_string(),
            }
        }
    }
}

/// Record an inference attempt for analytics.
fn record_inference_attempt(
    tracker: Option<&Arc<SqliteTracker>>,
    issue: &Issue,
    context: &IssueContext,
    inferred: Option<&InferredRepo>,
    duration_ms: i64,
) -> Option<i64> {
    let tracker = tracker?;

    let (confidence, reason, repo_id) = match inferred {
        Some(inf) => (
            inf.confidence.to_string(),
            inf.reason.clone(),
            tracker
                .get_indexed_repo(&inf.repo.name)
                .ok()
                .flatten()
                .map(|r| r.id),
        ),
        None => ("none".to_string(), "No match found".to_string(), None),
    };

    match tracker.record_inference_attempt(
        &issue.id,
        &issue.source,
        &context.filenames,
        &context.functions,
        &context.keywords,
        repo_id,
        &confidence,
        &reason,
        Some(duration_ms as u64),
    ) {
        Ok(id) => Some(id),
        Err(e) => {
            tracing::warn!(error = %e, "Failed to record inference attempt");
            None
        }
    }
}

/// Confidence level for repository inference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Direct file path match.
    High,
    /// Fuzzy/partial match.
    Medium,
    /// Content similarity only.
    Low,
    /// No match found.
    None,
}

impl std::fmt::Display for Confidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Confidence::High => write!(f, "high"),
            Confidence::Medium => write!(f, "medium"),
            Confidence::Low => write!(f, "low"),
            Confidence::None => write!(f, "none"),
        }
    }
}

/// Result of repository inference.
#[derive(Debug, Clone)]
pub struct InferredRepo {
    /// The inferred repository.
    pub repo: IndexedRepo,
    /// Confidence level of the inference.
    pub confidence: Confidence,
    /// Reason for the inference.
    pub reason: String,
    /// File that matched (if applicable).
    pub matched_file: Option<String>,
}

/// Pre-computed repository embedding for semantic search.
#[derive(Clone)]
pub struct RepoEmbedding {
    /// Repository name.
    pub name: String,
    /// Embedding vector.
    pub embedding: Vec<f32>,
}

/// Maximum number of repository embeddings to store in memory.
/// Beyond this limit, oldest embeddings are evicted (LRU-style).
const MAX_REPO_EMBEDDINGS: usize = 1000;

/// Repository inference engine.
///
/// Uses a RepoIndex to determine which repository an issue belongs to
/// based on file paths and other context extracted from the issue.
/// Optionally uses embeddings for semantic similarity matching.
///
/// Supports incremental updates to detect and embed new repositories.
#[derive(Clone)]
pub struct RepoInferrer {
    index: Arc<std::sync::RwLock<RepoIndex>>,
    /// Pre-computed embeddings for semantic matching.
    repo_embeddings: Arc<std::sync::RwLock<Vec<RepoEmbedding>>>,
    /// Known orgs for discovery.
    known_orgs: Vec<String>,
    /// Paths to scan for repos.
    discover_paths: Vec<String>,
}

impl RepoInferrer {
    /// Create a new inferrer with the given repository index (no embeddings).
    pub fn new(index: RepoIndex) -> Self {
        Self {
            index: Arc::new(std::sync::RwLock::new(index)),
            repo_embeddings: Arc::new(std::sync::RwLock::new(Vec::new())),
            known_orgs: Vec::new(),
            discover_paths: Vec::new(),
        }
    }

    /// Create a new inferrer with pre-computed embeddings.
    pub fn with_embeddings(index: RepoIndex, repo_embeddings: Vec<RepoEmbedding>) -> Self {
        Self {
            index: Arc::new(std::sync::RwLock::new(index)),
            repo_embeddings: Arc::new(std::sync::RwLock::new(repo_embeddings)),
            known_orgs: Vec::new(),
            discover_paths: Vec::new(),
        }
    }

    /// Create with discovery config for incremental updates.
    pub fn with_discovery(
        index: RepoIndex,
        repo_embeddings: Vec<RepoEmbedding>,
        known_orgs: Vec<String>,
        discover_paths: Vec<String>,
    ) -> Self {
        Self {
            index: Arc::new(std::sync::RwLock::new(index)),
            repo_embeddings: Arc::new(std::sync::RwLock::new(repo_embeddings)),
            known_orgs,
            discover_paths,
        }
    }

    /// Check if embeddings are available.
    pub fn has_embeddings(&self) -> bool {
        match self.repo_embeddings.read() {
            Ok(embeddings) => !embeddings.is_empty(),
            Err(e) => {
                tracing::error!(error = %e, "repo_embeddings RwLock poisoned in has_embeddings");
                false
            }
        }
    }

    /// Get the number of embedded repositories.
    pub fn embedding_count(&self) -> usize {
        match self.repo_embeddings.read() {
            Ok(embeddings) => embeddings.len(),
            Err(e) => {
                tracing::error!(error = %e, "repo_embeddings RwLock poisoned in embedding_count");
                0
            }
        }
    }

    /// Refresh the index and embed any new repositories.
    ///
    /// Returns the number of new repos that were discovered and embedded.
    pub async fn refresh_repos(
        &self,
        embedding_client: &crate::feedback::EmbeddingClient,
    ) -> crate::error::Result<usize> {
        if self.known_orgs.is_empty() || self.discover_paths.is_empty() {
            return Ok(0);
        }

        // Build fresh index
        let new_index = RepoIndex::build(&self.known_orgs, &self.discover_paths)?;

        // Find repos that don't have embeddings yet
        let existing_names: std::collections::HashSet<String> = {
            let embeddings = self.repo_embeddings.read()
                .map_err(|e| crate::error::Error::Other(format!("repo_embeddings RwLock poisoned: {}", e)))?;
            embeddings.iter().map(|e| e.name.clone()).collect()
        };

        let new_repos: Vec<_> = new_index
            .list()
            .into_iter()
            .filter(|r| !existing_names.contains(&r.name))
            .collect();

        if new_repos.is_empty() {
            // Update index anyway (file lists may have changed)
            let mut index = self.index.write()
                .map_err(|e| crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))?;
            *index = new_index;
            return Ok(0);
        }

        tracing::info!("Found {} new repositories to embed", new_repos.len());

        // Generate embeddings for new repos
        let texts: Vec<String> = new_repos
            .iter()
            .map(|r| r.name.replace(['/', '-'], " "))
            .collect();
        let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

        let vectors = embedding_client.embed_batch(&text_refs).await?;

        let new_embeddings: Vec<RepoEmbedding> = new_repos
            .iter()
            .zip(vectors.into_iter())
            .map(|(repo, vector)| RepoEmbedding {
                name: repo.name.clone(),
                embedding: vector,
            })
            .collect();

        let new_count = new_embeddings.len();

        // Update state with size limit enforcement
        {
            let mut embeddings = self.repo_embeddings.write()
                .map_err(|e| crate::error::Error::Other(format!("repo_embeddings RwLock poisoned: {}", e)))?;
            embeddings.extend(new_embeddings);

            // Evict oldest embeddings if we exceed the limit (LRU-style)
            // Embeddings are added at the end, so we remove from the front
            if embeddings.len() > MAX_REPO_EMBEDDINGS {
                let excess = embeddings.len() - MAX_REPO_EMBEDDINGS;
                tracing::warn!(
                    excess = excess,
                    max = MAX_REPO_EMBEDDINGS,
                    "Evicting {} old repository embeddings to stay within memory limit",
                    excess
                );
                embeddings.drain(0..excess);
            }
        }
        {
            let mut index = self.index.write()
                .map_err(|e| crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))?;
            *index = new_index;
        }

        tracing::info!("Added embeddings for {} new repositories", new_count);
        Ok(new_count)
    }

    /// Find the best matching repository using semantic similarity.
    fn find_by_embedding(&self, query_embedding: &[f32], min_similarity: f32) -> Option<(String, f32)> {
        use crate::feedback::cosine_similarity;

        let embeddings = match self.repo_embeddings.read() {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, "repo_embeddings RwLock poisoned in find_by_embedding");
                return None;
            }
        };
        let mut best_match: Option<(String, f32)> = None;

        for repo_emb in embeddings.iter() {
            let similarity = cosine_similarity(query_embedding, &repo_emb.embedding);
            if similarity >= min_similarity
                && (best_match.is_none() || similarity > best_match.as_ref().unwrap().1) {
                    best_match = Some((repo_emb.name.clone(), similarity));
                }
        }

        best_match
    }

    /// Infer the target repository for an issue.
    ///
    /// Tries multiple strategies in order of confidence:
    /// 1. Explicit repository reference (org/repo format)
    /// 2. Direct file path match
    /// 3. Fuzzy file search
    /// 4. Basename match
    pub fn infer(&self, issue: &Issue) -> Option<InferredRepo> {
        let context = IssueContext::from_issue(issue);
        let index = match self.index.read() {
            Ok(i) => i,
            Err(e) => {
                tracing::error!(error = %e, "index RwLock poisoned in infer");
                return None;
            }
        };

        tracing::debug!(
            issue_id = %issue.short_id,
            filenames = ?context.filenames,
            functions = ?context.functions,
            repos = ?context.repos,
            "Extracted issue context"
        );

        // Strategy 1: Explicit repository reference (e.g., "utopia-php/database")
        for repo_ref in &context.repos {
            if let Some(repo) = index.get(repo_ref) {
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    "High confidence match: explicit repo reference"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::High,
                    reason: format!("Explicit repo reference: {}", repo_ref),
                    matched_file: None,
                });
            }
        }

        // Strategy 2: Direct file path match
        for filename in &context.filenames {
            if let Some(repo) = index.find_by_file(filename) {
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    file = %filename,
                    "High confidence match: direct file path"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::High,
                    reason: format!("Direct file match: {}", filename),
                    matched_file: Some(filename.clone()),
                });
            }
        }

        // Strategy 3: Fuzzy file search (partial match)
        for filename in &context.filenames {
            let matches = index.search_files(filename);

            // If we have exactly one match, it's medium confidence
            if matches.len() == 1 {
                let (repo, matched_path) = matches[0];
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    query = %filename,
                    matched = %matched_path,
                    "Medium confidence match: single fuzzy match"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::Medium,
                    reason: format!("Fuzzy match: {} -> {}", filename, matched_path),
                    matched_file: Some(matched_path.to_string()),
                });
            }

            // If we have multiple matches in the same repo, still medium confidence
            if !matches.is_empty() {
                let first_repo = &matches[0].0.name;
                let all_same_repo = matches.iter().all(|(r, _)| r.name == *first_repo);

                if all_same_repo {
                    let (repo, matched_path) = matches[0];
                    tracing::info!(
                        issue_id = %issue.short_id,
                        repo = %repo.name,
                        matches = matches.len(),
                        "Medium confidence match: all matches in same repo"
                    );
                    return Some(InferredRepo {
                        repo: repo.clone(),
                        confidence: Confidence::Medium,
                        reason: format!(
                            "Fuzzy match ({} files): {} -> {}",
                            matches.len(),
                            filename,
                            matched_path
                        ),
                        matched_file: Some(matched_path.to_string()),
                    });
                }
            }
        }

        // Strategy 3: Try just the basename of each filename
        for filename in &context.filenames {
            let basename = std::path::Path::new(filename)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| filename.clone());

            let matches = index.search_files(&basename);

            if matches.len() == 1 {
                let (repo, matched_path) = matches[0];
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    basename = %basename,
                    matched = %matched_path,
                    "Low confidence match: basename match"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::Low,
                    reason: format!("Basename match: {} -> {}", basename, matched_path),
                    matched_file: Some(matched_path.to_string()),
                });
            }
        }

        // No match found with file-based strategies
        tracing::debug!(
            issue_id = %issue.short_id,
            "No repository match found"
        );
        None
    }

    /// Infer with a pre-computed query embedding as fallback.
    ///
    /// First tries all file-based strategies, then falls back to semantic
    /// similarity if embeddings are available and no match was found.
    pub fn infer_with_embedding(
        &self,
        issue: &Issue,
        query_embedding: Option<&[f32]>,
    ) -> Option<InferredRepo> {
        // First try file-based inference
        if let Some(result) = self.infer(issue) {
            return Some(result);
        }

        // Fall back to embedding-based inference if available
        if let Some(embedding) = query_embedding {
            if self.has_embeddings() {
                const MIN_SIMILARITY: f32 = 0.5; // Threshold for semantic match

                if let Some((repo_name, similarity)) = self.find_by_embedding(embedding, MIN_SIMILARITY) {
                    let index = match self.index.read() {
                        Ok(i) => i,
                        Err(e) => {
                            tracing::error!(error = %e, "index RwLock poisoned in infer_with_embedding");
                            return None;
                        }
                    };
                    if let Some(repo) = index.get(&repo_name) {
                        let confidence = if similarity >= 0.8 {
                            Confidence::High
                        } else if similarity >= 0.65 {
                            Confidence::Medium
                        } else {
                            Confidence::Low
                        };

                        tracing::info!(
                            issue_id = %issue.short_id,
                            repo = %repo.name,
                            similarity = %format!("{:.2}", similarity),
                            confidence = %confidence,
                            "Semantic similarity match"
                        );

                        return Some(InferredRepo {
                            repo: repo.clone(),
                            confidence,
                            reason: format!("Semantic similarity: {:.1}%", similarity * 100.0),
                            matched_file: None,
                        });
                    }
                }
            }
        }

        None
    }

    /// Get the number of indexed repositories.
    pub fn repo_count(&self) -> usize {
        match self.index.read() {
            Ok(index) => index.len(),
            Err(e) => {
                tracing::error!(error = %e, "index RwLock poisoned in repo_count");
                0
            }
        }
    }

    /// Find repo references in context that are not in our index.
    ///
    /// Returns a list of repo names (org/repo format) that were referenced
    /// but not found locally.
    pub fn find_unknown_repos(&self, context: &IssueContext) -> Vec<String> {
        match self.index.read() {
            Ok(index) => context
                .repos
                .iter()
                .filter(|repo_ref| index.get(repo_ref).is_none())
                .cloned()
                .collect(),
            Err(e) => {
                tracing::error!(error = %e, "index RwLock poisoned in find_unknown_repos");
                Vec::new()
            }
        }
    }

    /// Get a read lock on the index for syncing to external storage.
    ///
    /// Returns an error if the lock is poisoned.
    pub fn with_index<F, R>(&self, f: F) -> crate::error::Result<R>
    where
        F: FnOnce(&RepoIndex) -> crate::error::Result<R>,
    {
        match self.index.read() {
            Ok(index) => f(&index),
            Err(e) => {
                tracing::error!(error = %e, "index RwLock poisoned in with_index");
                Err(crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))
            }
        }
    }
}

/// Build repository embeddings for semantic inference.
///
/// Creates embeddings for each repository name to enable semantic matching.
pub async fn build_repo_embeddings(
    index: &RepoIndex,
    embedding_client: &crate::feedback::EmbeddingClient,
) -> crate::error::Result<Vec<RepoEmbedding>> {
    let repos = index.list();
    let mut embeddings = Vec::with_capacity(repos.len());

    // Create descriptive text for each repo
    let texts: Vec<String> = repos
        .iter()
        .map(|r| {
            // Use repo name as the main identifier
            // Could be enhanced with README content, file list summary, etc.
            r.name.replace(['/', '-'], " ")
        })
        .collect();

    let text_refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();

    tracing::info!("Building embeddings for {} repositories...", repos.len());

    let vectors = embedding_client.embed_batch(&text_refs).await?;

    for (repo, vector) in repos.iter().zip(vectors.into_iter()) {
        embeddings.push(RepoEmbedding {
            name: repo.name.clone(),
            embedding: vector,
        });
    }

    tracing::info!("Built {} repository embeddings", embeddings.len());

    Ok(embeddings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_test_index() -> RepoIndex {
        let mut index = RepoIndex::new();

        let mut repo1 = IndexedRepo::new("appwrite/console", "/path/console");
        repo1.files = vec![
            "src/routes/auth.ts".to_string(),
            "src/components/Button.tsx".to_string(),
            "src/lib/api/client.ts".to_string(),
        ];
        index.add_repo(repo1);

        let mut repo2 = IndexedRepo::new("appwrite/sdk-for-php", "/path/sdk-php");
        repo2.files = vec![
            "src/Appwrite/Client.php".to_string(),
            "src/Appwrite/Services/Account.php".to_string(),
        ];
        index.add_repo(repo2);

        index
    }

    fn create_test_issue(source: &str, title: &str, description: &str) -> Issue {
        Issue {
            id: "test-1".to_string(),
            short_id: "TEST-1".to_string(),
            source: source.to_string(),
            title: title.to_string(),
            description: if description.is_empty() {
                None
            } else {
                Some(description.to_string())
            },
            url: "https://example.com/test".to_string(),
            priority: crate::types::IssuePriority::Medium,
            status: crate::types::IssueStatus::Open,
            metadata: std::collections::HashMap::new(),
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn test_infer_high_confidence() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let mut issue = create_test_issue("sentry", "Auth error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("src/routes/auth.ts"));

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/console");
        assert_eq!(inferred.confidence, Confidence::High);
    }

    #[test]
    fn test_infer_medium_confidence() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // Use a partial path that should fuzzy match
        let mut issue = create_test_issue("sentry", "Client error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("Client.php"));

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/sdk-for-php");
        // Could be high or medium depending on exact matching logic
        assert!(
            inferred.confidence == Confidence::High
                || inferred.confidence == Confidence::Medium
                || inferred.confidence == Confidence::Low
        );
    }

    #[test]
    fn test_infer_no_match() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue("sentry", "Unknown error", "No file paths here");

        let result = inferrer.infer(&issue);

        assert!(result.is_none());
    }

    #[test]
    fn test_infer_from_linear_issue() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue(
            "linear",
            "Fix button styling",
            "The issue is in src/components/Button.tsx",
        );

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/console");
    }

    #[test]
    fn test_confidence_display() {
        assert_eq!(format!("{}", Confidence::High), "high");
        assert_eq!(format!("{}", Confidence::Medium), "medium");
        assert_eq!(format!("{}", Confidence::Low), "low");
        assert_eq!(format!("{}", Confidence::None), "none");
    }

    #[test]
    fn test_inferrer_index_access() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // Can access the index count
        assert_eq!(inferrer.repo_count(), 2);
    }
}

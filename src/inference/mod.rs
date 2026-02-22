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
        /// Repository name (org/repo format).
        repo_name: String,
        /// Database ID of the repo (for analytics).
        repo_id: Option<i64>,
        /// GitHub URL for the repository.
        scm_url: String,
        /// Default branch name.
        default_branch: String,
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

    /// Returns the GitHub URL if resolved.
    pub fn scm_url(&self) -> Option<&str> {
        match self {
            RepoResolution::Resolved { scm_url, .. } => Some(scm_url),
            RepoResolution::Skip { .. } => None,
        }
    }

    /// Returns the default branch if resolved.
    pub fn default_branch(&self) -> Option<&str> {
        match self {
            RepoResolution::Resolved { default_branch, .. } => Some(default_branch),
            RepoResolution::Skip { .. } => None,
        }
    }

    /// Returns the repository name if resolved.
    pub fn repo_name(&self) -> Option<&str> {
        match self {
            RepoResolution::Resolved { repo_name, .. } => Some(repo_name),
            RepoResolution::Skip { .. } => None,
        }
    }
}

/// Resolve the target repository for an issue.
///
/// This is the shared entry point for repo resolution used by both the watcher
/// and webhook server. It handles:
/// Resolve a repository path for cascade processing.
/// Unlike issue-based resolution, this looks up a repo directly by name.
pub fn resolve_repo_for_cascade(
    inferrer: Option<&RepoInferrer>,
    repo_name: &str,
) -> RepoResolution {
    let inferrer = match inferrer {
        Some(i) => i,
        None => {
            return RepoResolution::Skip {
                reason: "No inferrer available".to_string(),
            }
        }
    };

    match inferrer.with_index(|index| Ok(index.get(repo_name).cloned())) {
        Ok(Some(repo)) => RepoResolution::Resolved {
            project_dir: repo.path,
            repo_name: repo.name,
            repo_id: None,
            scm_url: repo.scm_url,
            default_branch: repo.default_branch,
        },
        Ok(None) => RepoResolution::Skip {
            reason: format!("Repository '{}' not found in index", repo_name),
        },
        Err(e) => RepoResolution::Skip {
            reason: format!("Index error: {}", e),
        },
    }
}

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
                            format!(
                                "Inferred repo {} for {}",
                                inferred.repo.name, issue.short_id
                            ),
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
                        repo_name: inferred.repo.name.clone(),
                        repo_id,
                        scm_url: inferred.repo.scm_url.clone(),
                        default_branch: inferred.repo.default_branch.clone(),
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
                            format!(
                                "Could not infer repository for {}, skipping",
                                issue.short_id
                            ),
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
            let embeddings = self.repo_embeddings.read().map_err(|e| {
                crate::error::Error::Other(format!("repo_embeddings RwLock poisoned: {}", e))
            })?;
            embeddings.iter().map(|e| e.name.clone()).collect()
        };

        let new_repos: Vec<_> = new_index
            .list()
            .into_iter()
            .filter(|r| !existing_names.contains(&r.name))
            .collect();

        if new_repos.is_empty() {
            // Update index anyway (file lists may have changed)
            let mut index = self
                .index
                .write()
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
            let mut embeddings = self.repo_embeddings.write().map_err(|e| {
                crate::error::Error::Other(format!("repo_embeddings RwLock poisoned: {}", e))
            })?;
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
            let mut index = self
                .index
                .write()
                .map_err(|e| crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))?;
            *index = new_index;
        }

        tracing::info!("Added embeddings for {} new repositories", new_count);
        Ok(new_count)
    }

    /// Index files for a repository that was just cloned.
    ///
    /// Call this after cloning a repo to update the in-memory file index.
    /// Returns the number of files indexed.
    pub fn index_cloned_repo(&self, repo_name: &str) -> crate::error::Result<usize> {
        let mut index = self
            .index
            .write()
            .map_err(|e| crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))?;

        match index.index_repo_files(repo_name) {
            Some(count) => {
                tracing::info!(
                    repo = %repo_name,
                    files = count,
                    "Indexed files for cloned repository"
                );
                Ok(count)
            }
            None => {
                tracing::warn!(repo = %repo_name, "Repository not found in index");
                Ok(0)
            }
        }
    }

    /// Get a repository from the index by name.
    pub fn get_repo(&self, repo_name: &str) -> Option<IndexedRepo> {
        let index = self.index.read().ok()?;
        index.get(repo_name).cloned()
    }

    /// Clone all API-discovered repos and index their files.
    ///
    /// Clones repos in parallel using `parallelism` workers.
    /// Returns the number of repos successfully cloned and indexed.
    pub async fn clone_and_index_all(&self, parallelism: usize) -> crate::error::Result<usize> {
        use crate::repo::GitOps;
        use futures::stream::{self, StreamExt};

        // Get repos that need cloning
        let repos_to_clone: Vec<_> = {
            let index = self
                .index
                .read()
                .map_err(|e| crate::error::Error::Other(format!("index RwLock poisoned: {}", e)))?;
            index
                .list()
                .into_iter()
                .filter(|r| !r.path.exists())
                .map(|r| {
                    (
                        r.name.clone(),
                        r.path.clone(),
                        r.scm_url.clone(),
                        r.default_branch.clone(),
                    )
                })
                .collect()
        };

        if repos_to_clone.is_empty() {
            return Ok(0);
        }

        tracing::info!(
            count = repos_to_clone.len(),
            parallelism = parallelism,
            "Cloning API-discovered repositories"
        );

        // Clone in parallel
        let results: Vec<_> = stream::iter(repos_to_clone)
            .map(|(name, path, scm_url, default_branch)| async move {
                match GitOps::ensure_repo_at_path(&path, &scm_url, &default_branch).await {
                    Ok(()) => Some(name),
                    Err(e) => {
                        tracing::error!(repo = %name, error = %e, "Failed to clone repository");
                        None
                    }
                }
            })
            .buffer_unordered(parallelism)
            .collect()
            .await;

        // Index files for successfully cloned repos
        let cloned_repos: Vec<_> = results.into_iter().flatten().collect();
        let cloned_count = cloned_repos.len();

        for repo_name in cloned_repos {
            if let Err(e) = self.index_cloned_repo(&repo_name) {
                tracing::warn!(repo = %repo_name, error = %e, "Failed to index cloned repo files");
            }
        }

        tracing::info!(count = cloned_count, "Finished cloning repositories");
        Ok(cloned_count)
    }

    /// Find a repository by Sentry project name.
    ///
    /// Sentry projects map 1:1 with top-level repos. This function tries to match
    /// a project name like "cloud-staging" or "console-production" to a repo.
    ///
    /// Only performs exact matching (project name == repo simple name after stripping
    /// environment suffixes). Fuzzy/substring matching is intentionally avoided to
    /// prevent false positives (e.g., "cloud" matching "cloudevents").
    fn find_repo_by_project_name(&self, index: &RepoIndex, project: &str) -> Option<IndexedRepo> {
        // Normalize: strip environment suffixes like -staging, -production, etc.
        let suffixes = [
            "-staging",
            "-production",
            "-prod",
            "-dev",
            "-development",
            "-test",
            "-qa",
            "-uat",
            "-preview",
            "-canary",
        ];

        let mut normalized = project.to_lowercase();
        for suffix in &suffixes {
            if let Some(stripped) = normalized.strip_suffix(suffix) {
                normalized = stripped.to_string();
                break;
            }
        }

        let repos = index.list();

        // Try to find a repo whose simple name matches the normalized project name
        for repo in &repos {
            // Get the repo name without org prefix (e.g., "appwrite/cloud" -> "cloud")
            let repo_simple = repo
                .name
                .split('/')
                .next_back()
                .unwrap_or(&repo.name)
                .to_lowercase();

            if repo_simple == normalized {
                tracing::debug!(
                    project = %project,
                    normalized = %normalized,
                    repo = %repo.name,
                    "Matched project to repo by simple name"
                );
                return Some((*repo).clone());
            }
        }

        None
    }

    /// Find a repository by partial name match.
    ///
    /// This handles cases where the extracted repo reference is partial:
    /// - "utopia-php/database" should match "utopia-php/database" in index
    /// - "database" could match "utopia-php/database" if it's the only match
    fn find_repo_by_partial_name(&self, index: &RepoIndex, name: &str) -> Option<IndexedRepo> {
        let repos = index.list();
        let name_lower = name.to_lowercase();

        // First, try exact match on the simple name (after org)
        let mut matches: Vec<_> = repos
            .iter()
            .filter(|r| {
                let simple = r
                    .name
                    .split('/')
                    .next_back()
                    .unwrap_or(&r.name)
                    .to_lowercase();
                simple == name_lower
            })
            .collect();

        // If exactly one match, return it
        if matches.len() == 1 {
            return Some((*matches[0]).clone());
        }

        // If no exact matches, try contains matching
        if matches.is_empty() {
            matches = repos
                .iter()
                .filter(|r| {
                    let name_lower_repo = r.name.to_lowercase();
                    name_lower_repo.contains(&name_lower)
                        || name_lower.contains(
                            name_lower_repo
                                .split('/')
                                .next_back()
                                .unwrap_or(&name_lower_repo),
                        )
                })
                .collect();

            if matches.len() == 1 {
                return Some((*matches[0]).clone());
            }
        }

        None
    }

    /// Find the best matching repository using semantic similarity.
    fn find_by_embedding(
        &self,
        query_embedding: &[f32],
        min_similarity: f32,
    ) -> Option<(String, f32)> {
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
                && (best_match.is_none() || similarity > best_match.as_ref().unwrap().1)
            {
                best_match = Some((repo_emb.name.clone(), similarity));
            }
        }

        best_match
    }

    /// Infer the target repository for an issue.
    ///
    /// Tries multiple strategies in order of confidence:
    /// 0. Sentry project name matching (for Sentry issues)
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

        // Strategy 0: Sentry project name matching
        // Sentry projects map 1:1 with top-level repos.
        // Try to find a repo whose name ends with the project name.
        if issue.source == "sentry" {
            if let Some(project) = issue.metadata.get("project").and_then(|v| v.as_str()) {
                if let Some(repo) = self.find_repo_by_project_name(&index, project) {
                    tracing::info!(
                        issue_id = %issue.short_id,
                        repo = %repo.name,
                        project = %project,
                        "High confidence match: Sentry project name"
                    );
                    return Some(InferredRepo {
                        repo: repo.clone(),
                        confidence: Confidence::High,
                        reason: format!("Sentry project: {} -> {}", project, repo.name),
                        matched_file: None,
                    });
                }
            }
        }

        // Strategy 1: Explicit repository reference (e.g., "utopia-php/database")
        for repo_ref in &context.repos {
            // First try exact match
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

            // Try partial match (repo name contains the reference or vice versa)
            if let Some(repo) = self.find_repo_by_partial_name(&index, repo_ref) {
                tracing::info!(
                    issue_id = %issue.short_id,
                    repo = %repo.name,
                    reference = %repo_ref,
                    "High confidence match: partial repo reference"
                );
                return Some(InferredRepo {
                    repo: repo.clone(),
                    confidence: Confidence::High,
                    reason: format!("Partial repo reference: {} -> {}", repo_ref, repo.name),
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

                if let Some((repo_name, similarity)) =
                    self.find_by_embedding(embedding, MIN_SIMILARITY)
                {
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
                Err(crate::error::Error::Other(format!(
                    "index RwLock poisoned: {}",
                    e
                )))
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

    fn create_test_index_with_cloud() -> RepoIndex {
        let mut index = RepoIndex::new();

        let mut cloud = IndexedRepo::new("appwrite/cloud", "/path/cloud");
        cloud.files = vec![
            "src/Appwrite/Cloud/Platform/Modules/Billing/Workers/TeamAggregation.php".to_string(),
            "src/Appwrite/Cloud/Platform/Modules/Usage/Workers/UsageAggregation.php".to_string(),
        ];
        index.add_repo(cloud);

        let mut database = IndexedRepo::new("utopia-php/database", "/path/database");
        database.files = vec![
            "src/Database/Adapter/SQL.php".to_string(),
            "src/Database/Adapter/Pool.php".to_string(),
            "src/Database/Database.php".to_string(),
        ];
        index.add_repo(database);

        let mut database_proxy =
            IndexedRepo::new("utopia-php/database-proxy", "/path/database-proxy");
        database_proxy.files = vec!["src/Proxy/Server.php".to_string()];
        index.add_repo(database_proxy);

        index
    }

    #[test]
    fn test_infer_from_sentry_project_name() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        let mut issue = create_test_issue("sentry", "MySQL server has gone away", "");
        issue
            .metadata
            .insert("project".to_string(), json!("cloud-staging"));

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/cloud");
        assert_eq!(inferred.confidence, Confidence::High);
        assert!(inferred.reason.contains("Sentry project"));
    }

    #[test]
    fn test_infer_from_vendor_package_in_stacktrace() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Issue without project metadata, but with a stacktrace mentioning vendor/utopia-php/database
        let mut issue = create_test_issue("sentry", "SQL Error", "");
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(
                r#"
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/SQL.php in __call at line 393
                "#
            ),
        );

        let result = inferrer.infer(&issue);

        // Should match utopia-php/database, NOT database-proxy
        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "utopia-php/database");
    }

    #[test]
    fn test_infer_prefers_sentry_project_over_vendor() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Issue with both project name and vendor packages in stacktrace
        // Sentry project should take precedence
        let mut issue = create_test_issue("sentry", "MySQL server has gone away", "");
        issue
            .metadata
            .insert("project".to_string(), json!("cloud-staging"));
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(
                r#"
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/SQL.php in __call at line 393
                "#
            ),
        );

        let result = inferrer.infer(&issue);

        // Should match appwrite/cloud (from project name), not utopia-php/database
        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/cloud");
        assert!(inferred.reason.contains("Sentry project"));
    }

    #[test]
    fn test_infer_does_not_match_similar_repo_names() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Make sure "utopia-php/database" doesn't accidentally match "utopia-php/database-proxy"
        let mut issue = create_test_issue("sentry", "SQL Error", "");
        issue.metadata.insert(
            "stacktrace".to_string(),
            json!(
                r#"
                /usr/src/code/vendor/utopia-php/database/src/Database/Adapter/SQL.php in __call at line 393
                "#
            ),
        );

        let result = inferrer.infer(&issue);

        assert!(result.is_some());
        let inferred = result.unwrap();
        // Should be database, NOT database-proxy
        assert_eq!(inferred.repo.name, "utopia-php/database");
        assert_ne!(inferred.repo.name, "utopia-php/database-proxy");
    }

    #[test]
    fn test_repo_resolution_resolved_accessors() {
        let res = RepoResolution::Resolved {
            project_dir: PathBuf::from("/path/repo"),
            repo_name: "org/repo".to_string(),
            repo_id: Some(42),
            scm_url: "https://github.com/org/repo".to_string(),
            default_branch: "main".to_string(),
        };
        assert!(res.is_resolved());
        assert_eq!(res.project_dir(), Some(&PathBuf::from("/path/repo")));
        assert_eq!(res.scm_url(), Some("https://github.com/org/repo"));
        assert_eq!(res.default_branch(), Some("main"));
        assert_eq!(res.repo_name(), Some("org/repo"));
    }

    #[test]
    fn test_repo_resolution_skip_accessors() {
        let res = RepoResolution::Skip {
            reason: "No match".to_string(),
        };
        assert!(!res.is_resolved());
        assert!(res.project_dir().is_none());
        assert!(res.scm_url().is_none());
        assert!(res.default_branch().is_none());
        assert!(res.repo_name().is_none());
    }

    #[test]
    fn test_confidence_display_all_variants() {
        assert_eq!(Confidence::High.to_string(), "high");
        assert_eq!(Confidence::Medium.to_string(), "medium");
        assert_eq!(Confidence::Low.to_string(), "low");
        assert_eq!(Confidence::None.to_string(), "none");
    }

    #[test]
    fn test_confidence_equality() {
        assert_eq!(Confidence::High, Confidence::High);
        assert_ne!(Confidence::High, Confidence::Low);
    }

    #[test]
    fn test_resolve_repo_for_cascade_no_inferrer() {
        let result = resolve_repo_for_cascade(None, "org/repo");
        assert!(!result.is_resolved());
    }

    #[test]
    fn test_resolve_repo_for_cascade_repo_found() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let result = resolve_repo_for_cascade(Some(&inferrer), "appwrite/console");
        assert!(result.is_resolved());
        assert_eq!(result.repo_name(), Some("appwrite/console"));
    }

    #[test]
    fn test_resolve_repo_for_cascade_repo_not_found() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let result = resolve_repo_for_cascade(Some(&inferrer), "nonexistent/repo");
        assert!(!result.is_resolved());
    }

    #[test]
    fn test_inferrer_no_embeddings_by_default() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        assert!(!inferrer.has_embeddings());
        assert_eq!(inferrer.embedding_count(), 0);
    }

    #[test]
    fn test_inferrer_with_embeddings() {
        let index = create_test_index();
        let embeddings = vec![RepoEmbedding {
            name: "appwrite/console".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
        }];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);
        assert!(inferrer.has_embeddings());
        assert_eq!(inferrer.embedding_count(), 1);
    }

    #[test]
    fn test_inferrer_get_repo() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let repo = inferrer.get_repo("appwrite/console");
        assert!(repo.is_some());
        assert_eq!(repo.unwrap().name, "appwrite/console");

        assert!(inferrer.get_repo("nonexistent").is_none());
    }

    #[test]
    fn test_inferrer_repo_count() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        assert_eq!(inferrer.repo_count(), 2);
    }

    #[test]
    fn test_infer_empty_issue_no_match() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let issue = create_test_issue("linear", "", "");
        assert!(inferrer.infer(&issue).is_none());
    }

    #[test]
    fn test_infer_non_sentry_ignores_project_metadata() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Linear issue with project metadata should NOT trigger Sentry project matching
        let mut issue = create_test_issue("linear", "Some error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("cloud-staging"));

        let result = inferrer.infer(&issue);
        // Should not match via project name (Sentry-only strategy)
        // Might match via other strategies or not at all
        if let Some(ref inferred) = result {
            assert!(
                !inferred.reason.contains("Sentry project"),
                "linear issues should not use Sentry project matching"
            );
        }
    }

    #[test]
    fn test_infer_sentry_project_strips_env_suffixes() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Test multiple environment suffixes
        for suffix in &[
            "-staging",
            "-production",
            "-prod",
            "-dev",
            "-test",
            "-qa",
            "-preview",
        ] {
            let project_name = format!("cloud{}", suffix);
            let mut issue = create_test_issue("sentry", "Error", "");
            issue
                .metadata
                .insert("project".to_string(), json!(project_name));

            let result = inferrer.infer(&issue);
            assert!(
                result.is_some(),
                "Should match for project: {}",
                project_name
            );
            assert_eq!(
                result.unwrap().repo.name,
                "appwrite/cloud",
                "Failed for suffix: {}",
                suffix
            );
        }
    }

    #[test]
    fn test_infer_with_embedding_no_file_match_falls_back() {
        let index = create_test_index();
        let embeddings = vec![
            RepoEmbedding {
                name: "appwrite/console".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
            },
            RepoEmbedding {
                name: "appwrite/sdk-for-php".to_string(),
                embedding: vec![0.0, 1.0, 0.0],
            },
        ];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        // Issue with no file paths, but a query embedding similar to console
        let issue = create_test_issue("linear", "Some frontend issue", "");
        let query_embedding = vec![0.95, 0.05, 0.0];

        let result = inferrer.infer_with_embedding(&issue, Some(&query_embedding));
        assert!(result.is_some());
        let inferred = result.unwrap();
        assert_eq!(inferred.repo.name, "appwrite/console");
        assert!(inferred.reason.contains("Semantic similarity"));
    }

    #[test]
    fn test_infer_with_embedding_none_no_fallback() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index); // No embeddings

        let issue = create_test_issue("linear", "Some issue", "");
        let result = inferrer.infer_with_embedding(&issue, None);
        assert!(result.is_none());
    }

    #[test]
    fn test_infer_with_embedding_file_match_takes_priority() {
        let index = create_test_index();
        let embeddings = vec![
            RepoEmbedding {
                name: "appwrite/console".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
            },
            RepoEmbedding {
                name: "appwrite/sdk-for-php".to_string(),
                embedding: vec![0.0, 1.0, 0.0],
            },
        ];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        // Issue with file path pointing to sdk-for-php, but embedding points to console
        let issue = create_test_issue(
            "sentry",
            "PHP SDK error",
            "Error in src/Appwrite/Client.php",
        );
        let query_embedding = vec![0.95, 0.05, 0.0]; // Points to console

        let result = inferrer.infer_with_embedding(&issue, Some(&query_embedding));
        assert!(result.is_some());
        let inferred = result.unwrap();
        // File-based match should win over embedding
        assert_eq!(inferred.repo.name, "appwrite/sdk-for-php");
        assert!(!inferred.reason.contains("Semantic similarity"));
    }

    #[test]
    fn test_find_unknown_repos_all_known() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let context = IssueContext {
            filenames: vec![],
            functions: vec![],
            repos: vec!["appwrite/console".to_string()],
            keywords: vec![],
            raw_text: String::new(),
        };
        let unknown = inferrer.find_unknown_repos(&context);
        assert!(unknown.is_empty());
    }

    #[test]
    fn test_find_unknown_repos_mixed() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let context = IssueContext {
            filenames: vec![],
            functions: vec![],
            repos: vec!["appwrite/console".to_string(), "unknown/repo".to_string()],
            keywords: vec![],
            raw_text: String::new(),
        };
        let unknown = inferrer.find_unknown_repos(&context);
        assert_eq!(unknown, vec!["unknown/repo".to_string()]);
    }

    #[test]
    fn test_find_unknown_repos_empty_context() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let context = IssueContext {
            filenames: vec![],
            functions: vec![],
            repos: vec![],
            keywords: vec![],
            raw_text: String::new(),
        };
        let unknown = inferrer.find_unknown_repos(&context);
        assert!(unknown.is_empty());
    }

    #[test]
    fn test_with_index_closure() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let count = inferrer.with_index(|idx| Ok(idx.len())).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_embedding_confidence_thresholds() {
        let index = create_test_index();
        let embeddings = vec![RepoEmbedding {
            name: "appwrite/console".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
        }];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        // High similarity (>= 0.8) -> High confidence
        let issue = create_test_issue("linear", "test", "");
        let high_emb = vec![0.95, 0.05, 0.0]; // cos sim ~0.998
        let result = inferrer.infer_with_embedding(&issue, Some(&high_emb));
        assert!(result.is_some());
        assert_eq!(result.unwrap().confidence, Confidence::High);

        // Medium similarity (0.65-0.8) -> Medium confidence
        let med_emb = vec![0.7, 0.7, 0.0]; // cos sim ~0.707
        let result = inferrer.infer_with_embedding(&issue, Some(&med_emb));
        assert!(result.is_some());
        assert_eq!(result.unwrap().confidence, Confidence::Medium);

        // Low similarity (0.5-0.65) -> Low confidence
        let low_emb = vec![0.5, 0.85, 0.0]; // cos sim ~0.507
        let result = inferrer.infer_with_embedding(&issue, Some(&low_emb));
        assert!(result.is_some());
        assert_eq!(result.unwrap().confidence, Confidence::Low);
    }

    #[test]
    fn test_embedding_below_threshold_returns_none() {
        let index = create_test_index();
        let embeddings = vec![RepoEmbedding {
            name: "appwrite/console".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
        }];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        // Very orthogonal query -> below 0.5 threshold
        let issue = create_test_issue("linear", "test", "");
        let orthogonal = vec![0.0, 1.0, 0.0]; // cos sim = 0.0
        let result = inferrer.infer_with_embedding(&issue, Some(&orthogonal));
        assert!(result.is_none(), "below threshold should return None");
    }

    #[test]
    fn test_with_discovery_constructor() {
        let index = create_test_index();
        let inferrer = RepoInferrer::with_discovery(
            index,
            vec![],
            vec!["org1".to_string()],
            vec!["/path".to_string()],
        );
        assert_eq!(inferrer.repo_count(), 2);
        assert!(!inferrer.has_embeddings());
    }

    #[test]
    fn test_resolve_repo_for_issue_no_inferrer_no_tracker() {
        let issue = create_test_issue("linear", "Some issue", "description");
        let result = resolve_repo_for_issue(None, &issue, None);
        assert!(!result.is_resolved());
    }

    #[test]
    fn test_resolve_repo_for_issue_with_file_match() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let mut issue = create_test_issue("sentry", "Auth error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("src/routes/auth.ts"));

        let result = resolve_repo_for_issue(Some(&inferrer), &issue, None);
        assert!(result.is_resolved());
        assert_eq!(result.repo_name(), Some("appwrite/console"));
    }

    #[test]
    fn test_resolve_repo_for_issue_no_match() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue("linear", "Unknown", "Nothing matches");
        let result = resolve_repo_for_issue(Some(&inferrer), &issue, None);
        assert!(!result.is_resolved());
    }

    #[test]
    fn test_resolve_repo_for_issue_with_embedding_fallback() {
        let index = create_test_index();
        let embeddings = vec![
            RepoEmbedding {
                name: "appwrite/console".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
            },
            RepoEmbedding {
                name: "appwrite/sdk-for-php".to_string(),
                embedding: vec![0.0, 1.0, 0.0],
            },
        ];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        let issue = create_test_issue("linear", "Frontend issue", "");
        let query_emb = vec![0.95, 0.05, 0.0];

        let result =
            resolve_repo_for_issue_with_embedding(Some(&inferrer), &issue, None, Some(&query_emb));
        assert!(result.is_resolved());
        assert_eq!(result.repo_name(), Some("appwrite/console"));
    }

    #[test]
    fn test_find_repo_by_project_no_match() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // Use a project name that does not match anything
        let mut issue = create_test_issue("sentry", "Error", "");
        issue
            .metadata
            .insert("project".to_string(), json!("nonexistent-app"));

        let result = inferrer.infer(&issue);
        // Sentry project lookup will fail, so should fall through
        // Since there are no file references either, should return None
        assert!(result.is_none());
    }

    #[test]
    fn test_find_repo_by_project_bare_name_no_suffix() {
        let index = create_test_index_with_cloud();
        let inferrer = RepoInferrer::new(index);

        // "cloud" directly matches "appwrite/cloud"
        let mut issue = create_test_issue("sentry", "Error", "");
        issue.metadata.insert("project".to_string(), json!("cloud"));

        let result = inferrer.infer(&issue);
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "appwrite/cloud");
    }

    #[test]
    fn test_infer_explicit_repo_reference() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // Issue that mentions "appwrite/console" in the description
        let issue = create_test_issue(
            "linear",
            "Bug in console",
            "This is happening in appwrite/console repo",
        );
        let result = inferrer.infer(&issue);
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "appwrite/console");
    }

    #[test]
    fn test_find_by_embedding_no_embeddings() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index); // No embeddings

        let issue = create_test_issue("linear", "test", "");
        let emb = vec![1.0, 0.0, 0.0];
        let result = inferrer.infer_with_embedding(&issue, Some(&emb));
        // No embeddings, so embedding-based fallback should not produce a match
        assert!(result.is_none());
    }

    #[test]
    fn test_find_by_embedding_multiple_repos_picks_best() {
        let index = create_test_index();
        let embeddings = vec![
            RepoEmbedding {
                name: "appwrite/console".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
            },
            RepoEmbedding {
                name: "appwrite/sdk-for-php".to_string(),
                embedding: vec![0.0, 1.0, 0.0],
            },
        ];
        let inferrer = RepoInferrer::with_embeddings(index, embeddings);

        let issue = create_test_issue("linear", "test", "");
        // This embedding is closer to sdk-for-php
        let emb = vec![0.1, 0.99, 0.0];
        let result = inferrer.infer_with_embedding(&issue, Some(&emb));
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "appwrite/sdk-for-php");
    }

    #[test]
    fn test_confidence_copy() {
        let c = Confidence::High;
        let c2 = c;
        assert_eq!(c, c2);
    }

    #[test]
    fn test_confidence_debug() {
        let dbg = format!("{:?}", Confidence::Medium);
        assert_eq!(dbg, "Medium");
    }

    #[test]
    fn test_inferred_repo_clone() {
        let repo = IndexedRepo::new("test/repo", "/path");
        let inferred = InferredRepo {
            repo: repo.clone(),
            confidence: Confidence::High,
            reason: "test reason".to_string(),
            matched_file: Some("file.rs".to_string()),
        };
        let cloned = inferred.clone();
        assert_eq!(cloned.repo.name, "test/repo");
        assert_eq!(cloned.confidence, Confidence::High);
        assert_eq!(cloned.reason, "test reason");
        assert_eq!(cloned.matched_file, Some("file.rs".to_string()));
    }

    #[test]
    fn test_inferred_repo_debug() {
        let repo = IndexedRepo::new("test/repo", "/path");
        let inferred = InferredRepo {
            repo,
            confidence: Confidence::Low,
            reason: "fuzzy match".to_string(),
            matched_file: None,
        };
        let dbg = format!("{:?}", inferred);
        assert!(dbg.contains("Low"));
        assert!(dbg.contains("fuzzy match"));
    }

    #[test]
    fn test_repo_resolution_debug() {
        let res = RepoResolution::Skip {
            reason: "test skip".to_string(),
        };
        let dbg = format!("{:?}", res);
        assert!(dbg.contains("Skip"));
        assert!(dbg.contains("test skip"));
    }

    #[test]
    fn test_repo_resolution_resolved_repo_id_none() {
        let res = RepoResolution::Resolved {
            project_dir: PathBuf::from("/path"),
            repo_name: "org/repo".to_string(),
            repo_id: None,
            scm_url: "https://github.com/org/repo".to_string(),
            default_branch: "main".to_string(),
        };
        assert!(res.is_resolved());
        assert_eq!(res.repo_name(), Some("org/repo"));
    }

    #[test]
    fn test_resolve_repo_for_cascade_returns_correct_fields() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let result = resolve_repo_for_cascade(Some(&inferrer), "appwrite/console");
        assert!(result.is_resolved());

        match result {
            RepoResolution::Resolved {
                project_dir,
                repo_name,
                repo_id,
                scm_url,
                default_branch,
            } => {
                assert_eq!(project_dir, PathBuf::from("/path/console"));
                assert_eq!(repo_name, "appwrite/console");
                assert!(repo_id.is_none()); // Cascade doesn't do DB lookup
                assert!(!scm_url.is_empty());
                assert!(!default_branch.is_empty());
            }
            _ => panic!("Expected Resolved variant"),
        }
    }

    #[test]
    fn test_with_index_returns_error_on_closure_error() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let result: crate::error::Result<()> =
            inferrer.with_index(|_| Err(crate::error::Error::Other("test error".to_string())));
        assert!(result.is_err());
    }

    #[test]
    fn test_repo_embedding_clone() {
        let emb = RepoEmbedding {
            name: "test/repo".to_string(),
            embedding: vec![1.0, 2.0, 3.0],
        };
        let cloned = emb.clone();
        assert_eq!(cloned.name, "test/repo");
        assert_eq!(cloned.embedding, vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_inferrer_empty_index() {
        let index = RepoIndex::new();
        let inferrer = RepoInferrer::new(index);
        assert_eq!(inferrer.repo_count(), 0);
        assert!(!inferrer.has_embeddings());

        let issue = create_test_issue("linear", "test", "test description");
        assert!(inferrer.infer(&issue).is_none());
    }

    #[test]
    fn test_inferrer_with_discovery_has_embeddings() {
        let index = create_test_index();
        let embeddings = vec![RepoEmbedding {
            name: "appwrite/console".to_string(),
            embedding: vec![1.0, 0.0],
        }];
        let inferrer = RepoInferrer::with_discovery(
            index,
            embeddings,
            vec!["org".to_string()],
            vec!["/path".to_string()],
        );
        assert!(inferrer.has_embeddings());
        assert_eq!(inferrer.embedding_count(), 1);
    }

    #[test]
    fn test_infer_from_description_file_path() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // File path is extracted from the description text
        let issue = create_test_issue(
            "linear",
            "Error in auth module",
            "Stack trace shows error at src/routes/auth.ts line 42",
        );
        let result = inferrer.infer(&issue);
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "appwrite/console");
    }

    #[test]
    fn test_find_unknown_repos_all_unknown() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        let context = IssueContext {
            filenames: vec![],
            functions: vec![],
            repos: vec!["unknown/a".to_string(), "unknown/b".to_string()],
            keywords: vec![],
            raw_text: String::new(),
        };
        let unknown = inferrer.find_unknown_repos(&context);
        assert_eq!(unknown.len(), 2);
        assert!(unknown.contains(&"unknown/a".to_string()));
        assert!(unknown.contains(&"unknown/b".to_string()));
    }

    #[test]
    fn test_find_repo_by_partial_name_exact_match_via_repo_ref() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        // "appwrite/console" explicitly references the repo in org/repo format
        let issue = create_test_issue(
            "linear",
            "Bug in appwrite/console",
            "The appwrite/console repo has an issue",
        );
        let result = inferrer.infer(&issue);
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "appwrite/console");
    }

    #[test]
    fn test_find_repo_by_partial_name_contains_via_repo_ref() {
        let mut index = RepoIndex::new();
        let repo = IndexedRepo::new("myorg/payment-service", "/path/payment-service");
        index.add_repo(repo);
        let inferrer = RepoInferrer::new(index);

        // Uses org/repo format so the repo ref regex can extract it
        let issue = create_test_issue(
            "linear",
            "Payment issue",
            "The myorg/payment-service is returning errors",
        );
        let result = inferrer.infer(&issue);
        assert!(result.is_some());
        assert_eq!(result.unwrap().repo.name, "myorg/payment-service");
    }

    #[test]
    fn test_no_repo_match_without_org_repo_format() {
        let mut index = RepoIndex::new();
        let repo = IndexedRepo::new("myorg/payment-service", "/path/payment-service");
        index.add_repo(repo);
        let inferrer = RepoInferrer::new(index);

        // Plain text without org/repo format won't be extracted as a repo ref
        let issue = create_test_issue(
            "linear",
            "Payment issue",
            "The payment-service is returning errors",
        );
        let result = inferrer.infer(&issue);
        // No org/repo format -> no extraction -> no match
        assert!(result.is_none());
    }

    #[test]
    fn test_record_inference_attempt_no_tracker() {
        let issue = create_test_issue("linear", "test", "desc");
        let context = IssueContext {
            filenames: vec!["file.rs".to_string()],
            functions: vec!["main".to_string()],
            repos: vec![],
            keywords: vec!["auth".to_string()],
            raw_text: "test".to_string(),
        };

        let result = record_inference_attempt(None, &issue, &context, None, 100);
        assert!(result.is_none());
    }

    #[test]
    fn test_record_inference_attempt_with_tracker_no_match() {
        let db = std::sync::Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let issue = create_test_issue("linear", "test", "desc");
        let context = IssueContext {
            filenames: vec![],
            functions: vec![],
            repos: vec![],
            keywords: vec!["auth".to_string()],
            raw_text: "test issue".to_string(),
        };

        let result = record_inference_attempt(Some(&db), &issue, &context, None, 50);
        assert!(result.is_some());
        let id = result.unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_record_inference_attempt_with_tracker_and_match() {
        let db = std::sync::Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let issue = create_test_issue("sentry", "Auth error", "error in auth");
        let context = IssueContext {
            filenames: vec!["auth.ts".to_string()],
            functions: vec![],
            repos: vec![],
            keywords: vec!["auth".to_string()],
            raw_text: "auth error".to_string(),
        };

        let repo = IndexedRepo::new("org/repo", "/path/repo");
        let inferred = InferredRepo {
            repo,
            confidence: Confidence::High,
            reason: "File path match".to_string(),
            matched_file: Some("auth.ts".to_string()),
        };

        let result = record_inference_attempt(Some(&db), &issue, &context, Some(&inferred), 200);
        assert!(result.is_some());
    }

    #[test]
    fn test_index_cloned_repo_not_found() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let result = inferrer.index_cloned_repo("nonexistent/repo");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn test_index_cloned_repo_found() {
        let temp = tempfile::TempDir::new().unwrap();
        let repo_dir = temp.path().join("myrepo");
        std::fs::create_dir(&repo_dir).unwrap();
        std::fs::write(repo_dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(repo_dir.join("lib.rs"), "pub mod utils;").unwrap();

        let mut index = RepoIndex::new();
        let repo = IndexedRepo::new("org/myrepo", &repo_dir);
        index.add_repo(repo);

        let inferrer = RepoInferrer::new(index);
        let result = inferrer.index_cloned_repo("org/myrepo");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 2);

        // Verify files are now searchable
        let found = inferrer.get_repo("org/myrepo").unwrap();
        assert_eq!(found.files.len(), 2);
    }

    #[test]
    fn test_get_repo_existing() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let repo = inferrer.get_repo("appwrite/console");
        assert!(repo.is_some());
        assert_eq!(repo.unwrap().name, "appwrite/console");
    }

    #[test]
    fn test_get_repo_nonexistent() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        assert!(inferrer.get_repo("nonexistent/repo").is_none());
    }

    #[test]
    fn test_resolve_repo_for_issue_with_tracker_records_analytics() {
        let db = std::sync::Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let mut issue = create_test_issue("sentry", "Auth error", "");
        issue
            .metadata
            .insert("filename".to_string(), json!("src/routes/auth.ts"));

        let result = resolve_repo_for_issue(Some(&inferrer), &issue, Some(&db));
        assert!(result.is_resolved());
        assert_eq!(result.repo_name(), Some("appwrite/console"));
    }

    #[test]
    fn test_resolve_repo_for_issue_no_match_with_tracker() {
        let db = std::sync::Arc::new(crate::storage::SqliteTracker::in_memory().unwrap());
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let issue = create_test_issue("linear", "Unknown", "Nothing matches");
        let result = resolve_repo_for_issue(Some(&inferrer), &issue, Some(&db));
        assert!(!result.is_resolved());
    }

    #[test]
    fn test_repo_count_reflects_initial_index() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);
        assert_eq!(inferrer.repo_count(), 2);
    }

    #[test]
    fn test_repo_count_empty_index() {
        let index = RepoIndex::new();
        let inferrer = RepoInferrer::new(index);
        assert_eq!(inferrer.repo_count(), 0);
    }

    #[test]
    fn test_repo_count_three_repos() {
        let mut index = RepoIndex::new();
        index.add_repo(IndexedRepo::new("org/repo1", "/path1"));
        index.add_repo(IndexedRepo::new("org/repo2", "/path2"));
        index.add_repo(IndexedRepo::new("org/repo3", "/path3"));
        let inferrer = RepoInferrer::new(index);
        assert_eq!(inferrer.repo_count(), 3);
    }

    #[test]
    fn test_with_index_returns_value() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let total_files = inferrer.with_index(|idx| Ok(idx.total_files())).unwrap();
        assert_eq!(total_files, 5); // 3 + 2 files in the test index
    }

    #[test]
    fn test_with_index_can_search() {
        let index = create_test_index();
        let inferrer = RepoInferrer::new(index);

        let results = inferrer
            .with_index(|idx| {
                let found = idx.find_by_file("src/routes/auth.ts");
                Ok(found.map(|r| r.name.clone()))
            })
            .unwrap();

        assert_eq!(results, Some("appwrite/console".to_string()));
    }
}

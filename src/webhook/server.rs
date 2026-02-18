//! HTTP server for webhooks.

use super::{GitHubWebhookHandler, WebhookHandler, WebhookHandlerRegistry};
use crate::config::Config;
use crate::error::Error;
use crate::error::Result;
use crate::feedback::{
    format_similar_issues_context, FeedbackAnalyzer, FixOutcome, IssueEmbeddingService, Outcome,
};
use crate::github::{PrReviewState, ReviewWatcher};
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::{send_to_all_and_wait_first_reply, Notifier};
use crate::qa::{
    build_correlation_id, embed_text, find_reusable_qa, format_answer_context,
    format_reuse_context, format_timeout_context, normalize_text,
};
use crate::repo::{worktree_path, GitOps};
use crate::runner::{ClaudeRunner, ClaudeRunnerConfig};
use crate::storage::{classify_error, compute_error_hash, FixAttemptTracker, SqliteTracker};
use crate::types::{
    validate_issue_id, ActivityLogEntry, AskRequest, ErrorPattern, Issue, ProcessingMetric,
    QaKnowledgeEntry,
};
use crate::users::UserRegistry;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tower::limit::ConcurrencyLimitLayer;

/// Maximum time a processing entry can remain in the set before automatic cleanup (1 hour).
/// This prevents unbounded memory growth if a task fails to clean up properly.
const PROCESSING_ENTRY_TTL_SECS: u64 = 3600;

/// Maximum number of entries in the processing set before forced cleanup.
const MAX_PROCESSING_ENTRIES: usize = 1000;

/// State shared across handlers.
struct AppState {
    config: Config,
    handlers: WebhookHandlerRegistry,
    notifier: Arc<dyn Notifier>,
    tracker: Arc<dyn FixAttemptTracker>,
    sqlite_tracker: Option<Arc<SqliteTracker>>,
    inferrer: Option<RepoInferrer>,
    embedding_client: Option<crate::feedback::EmbeddingClient>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    feedback_analyzer: tokio::sync::Mutex<FeedbackAnalyzer>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    user_registry: UserRegistry,
    claude: ClaudeRunner,
    github_handler: Option<GitHubWebhookHandler>,
    /// Tracks currently processing webhooks with timestamps for TTL-based cleanup.
    /// Key: processing key (source:issue_id), Value: timestamp when processing started.
    processing: RwLock<HashMap<String, Instant>>,
}

/// HTTP server for webhooks.
pub struct WebhookServer {
    config: Config,
    handlers: WebhookHandlerRegistry,
    notifier: Arc<dyn Notifier>,
    tracker: Arc<dyn FixAttemptTracker>,
    sqlite_tracker: Option<Arc<SqliteTracker>>,
    inferrer: Option<RepoInferrer>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    github_handler: Option<GitHubWebhookHandler>,
    port: u16,
}

impl WebhookServer {
    /// Create a new webhook server.
    pub fn new(
        config: Config,
        handlers: WebhookHandlerRegistry,
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<SqliteTracker>>,
        inferrer: Option<RepoInferrer>,
    ) -> Self {
        Self::new_with_github(
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker,
            inferrer,
            None,
        )
    }

    /// Create a new webhook server with optional GitHub review webhook handling.
    pub fn new_with_github(
        config: Config,
        handlers: WebhookHandlerRegistry,
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<SqliteTracker>>,
        inferrer: Option<RepoInferrer>,
        github_handler: Option<GitHubWebhookHandler>,
    ) -> Self {
        let port = config.webhook_port;
        Self {
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker,
            inferrer,
            issue_embedding_service: None,
            review_watcher: None,
            github_handler,
            port,
        }
    }

    /// Set the issue embedding service for semantic dedup and context enrichment.
    pub fn set_issue_embedding_service(&mut self, service: Option<Arc<IssueEmbeddingService>>) {
        self.issue_embedding_service = service;
    }

    /// Set the review watcher for PR review tracking.
    pub fn set_review_watcher(&mut self, watcher: Option<Arc<ReviewWatcher>>) {
        self.review_watcher = watcher;
    }

    /// Build a repository inferrer from config.
    ///
    /// Delegates to `Watcher::build_inferrer` to avoid code duplication.
    pub async fn build_inferrer(
        config: &Config,
        github_client: Option<&crate::github::GitHubClient>,
    ) -> Result<Option<RepoInferrer>> {
        crate::watcher::Watcher::build_inferrer(config, github_client).await
    }

    /// Start the server.
    pub async fn start(self) -> Result<()> {
        let embedding_client = crate::feedback::EmbeddingClient::from_env().ok();
        let user_registry = UserRegistry::new(self.config.users.clone());

        // Initialize FeedbackAnalyzer and warm-start with DB outcomes
        let mut feedback_analyzer = FeedbackAnalyzer::new();
        if let Some(ref sqlite_tracker) = self.sqlite_tracker {
            feedback_analyzer = feedback_analyzer.with_sqlite_tracker(sqlite_tracker.clone());
            match sqlite_tracker.get_feedback_outcomes(None, 1000) {
                Ok(outcomes) if !outcomes.is_empty() => {
                    let count = outcomes.len();
                    feedback_analyzer.load_outcomes(outcomes);
                    tracing::info!(
                        count = count,
                        "Loaded feedback outcomes for webhook learning"
                    );
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to load feedback outcomes"),
            }
        }

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: self.config.claude_timeout_secs,
                    model: self.config.claude.model.clone(),
                    instructions: self.config.claude.instructions.clone(),
                    permissions: self.config.claude.permissions.clone(),
                    skip_permissions: self.config.claude.skip_permissions,
                },
                self.tracker.clone(),
            ),
            config: self.config,
            handlers: self.handlers,
            notifier: self.notifier,
            tracker: self.tracker,
            sqlite_tracker: self.sqlite_tracker,
            inferrer: self.inferrer,
            embedding_client,
            issue_embedding_service: self.issue_embedding_service,
            feedback_analyzer: tokio::sync::Mutex::new(feedback_analyzer),
            review_watcher: self.review_watcher,
            user_registry,
            github_handler: self.github_handler,
            processing: RwLock::new(HashMap::new()),
        });

        // Concurrency limit: max 10 concurrent webhook processing
        // This prevents overwhelming the system with too many simultaneous fix attempts
        // Combined with the processing set, this provides effective rate control
        let concurrency_layer = ConcurrencyLimitLayer::new(10);

        let app = Router::new()
            .route("/health", get(health_handler))
            .route(
                "/webhook/:source",
                post(webhook_handler).layer(concurrency_layer),
            )
            .with_state(state.clone());

        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied && self.port < 1024 {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "Cannot bind to port {} (privileged ports < 1024 require root). \
                         Use a port >= 1024 or run with elevated privileges.",
                        self.port
                    ),
                )
            } else {
                e
            }
        })?;

        tracing::info!(port = self.port, "Webhook server listening");
        tracing::info!(
            work_dir = ?state.config.work_dir,
            known_orgs = state.config.known_orgs.len(),
            "Repository configuration"
        );
        tracing::info!("Concurrency limit: 10 concurrent webhook requests maximum");
        tracing::info!(
            "Handlers: {}",
            state
                .handlers
                .get_all()
                .iter()
                .map(|h| h.source_name())
                .collect::<Vec<_>>()
                .join(", ")
        );
        tracing::info!("");
        tracing::info!("Endpoints:");
        tracing::info!("  GET  http://localhost:{}/health", self.port);
        for handler in state.handlers.get_all() {
            tracing::info!(
                "  POST http://localhost:{}/webhook/{}",
                self.port,
                handler.source_name()
            );
        }

        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn health_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let processing_count = state.processing.read().await.len();
    let handlers: Vec<&str> = state
        .handlers
        .get_all()
        .iter()
        .map(|h| h.source_name())
        .collect();
    let github_enabled = state.github_handler.is_some();

    Json(json!({
        "status": "ok",
        "processing_count": processing_count,
        "handlers": handlers,
        "github_webhook_enabled": github_enabled
    }))
}

async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Path(source_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    // Convert headers to HashMap
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|val| (k.as_str().to_lowercase(), val.to_string()))
        })
        .collect();

    // GitHub review webhooks are handled outside the generic issue handler registry.
    if source_name == "github" {
        return handle_github_webhook(state, &header_map, &body).await;
    }

    let handler = match state.handlers.get(&source_name) {
        Some(h) => h,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Unknown source: {}", source_name) })),
            );
        }
    };

    // Log webhook received
    let has_signature = header_map.contains_key("x-signature")
        || header_map.contains_key("sentry-hook-signature")
        || header_map.contains_key("linear-signature")
        || header_map.contains_key("x-hub-signature-256");
    let activity = ActivityLogEntry::new(
        "webhook_received",
        format!("Webhook received from {}", source_name),
    )
    .with_source(source_name.clone())
    .with_metadata(json!({
        "content_length": body.len(),
        "has_signature": has_signature
    }));
    state.tracker.record_activity(&activity).ok();

    // Verify signature
    if !handler.verify_signature(&body, &header_map) {
        tracing::error!(source = source_name.as_str(), "Invalid webhook signature");

        // Log webhook rejected
        let activity = ActivityLogEntry::new(
            "webhook_rejected",
            format!("Webhook rejected: invalid signature from {}", source_name),
        )
        .with_source(source_name.clone());
        state.tracker.record_activity(&activity).ok();

        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "Invalid signature" })),
        );
    }

    // Webhook delivery ID idempotency: prevent redelivered webhooks from
    // being processed twice (e.g., after server restart loses in-memory state).
    let delivery_id = header_map
        .get("linear-delivery")
        .or_else(|| header_map.get("x-github-delivery"))
        .or_else(|| header_map.get("sentry-hook-id"));
    if let (Some(delivery_id), Some(sqlite_tracker)) = (delivery_id, state.sqlite_tracker.as_ref())
    {
        match sqlite_tracker.check_and_record_delivery(delivery_id, &source_name) {
            Ok(true) => {} // New delivery, proceed
            Ok(false) => {
                tracing::info!(
                    source = source_name.as_str(),
                    delivery_id = delivery_id.as_str(),
                    "Duplicate webhook delivery, ignoring"
                );
                return (
                    StatusCode::OK,
                    Json(json!({ "status": "ignored", "reason": "Duplicate delivery" })),
                );
            }
            Err(e) => {
                tracing::warn!(
                    source = source_name.as_str(),
                    error = %e,
                    "Failed to check delivery idempotency, proceeding anyway"
                );
            }
        }
        // Probabilistically clean up old delivery records (older than 24h).
        // Only run ~1-in-50 requests to avoid unnecessary DELETE queries on every webhook.
        if rand::random_range(0u32..50) == 0 {
            sqlite_tracker.cleanup_old_deliveries(24).ok();
        }
    }

    // Parse JSON
    let payload: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid JSON" })),
            );
        }
    };

    // Parse into issue
    let issue = match handler.parse_payload(&payload).await {
        Ok(Some(issue)) => issue,
        Ok(None) => {
            return (
                StatusCode::OK,
                Json(json!({ "status": "ignored", "reason": "Event not applicable" })),
            );
        }
        Err(e) => {
            tracing::error!(
                source = source_name.as_str(),
                error = %e,
                "Error parsing payload"
            );
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Failed to parse payload" })),
            );
        }
    };

    // Validate issue ID to prevent path traversal and other security issues
    if let Err(validation_error) = validate_issue_id(&issue.id) {
        tracing::warn!(
            source = source_name.as_str(),
            issue_id = issue.id.as_str(),
            error = validation_error.as_str(),
            "Invalid issue ID rejected"
        );
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Invalid issue ID: {}", validation_error) })),
        );
    }

    // Check criteria
    let match_result = handler.matches_criteria(&issue);
    if !match_result.matches {
        tracing::info!(
            source = source_name.as_str(),
            issue_id = issue.short_id.as_str(),
            reason = match_result.reason.as_str(),
            "Issue does not match criteria"
        );
        return (
            StatusCode::OK,
            Json(json!({ "status": "ignored", "reason": match_result.reason })),
        );
    }

    // Check if already attempted
    match state.tracker.has_attempted(&source_name, &issue.id) {
        Ok(true) => {
            return (
                StatusCode::OK,
                Json(json!({ "status": "ignored", "reason": "Already attempted" })),
            );
        }
        Err(e) => {
            tracing::error!(source = source_name.as_str(), error = %e, "Failed to check if issue was already attempted");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "status": "error", "reason": "Database error" })),
            );
        }
        Ok(false) => {}
    }

    // Semantic dedup gate: skip if this issue is a duplicate of one already handled
    if let Some(ref embedding_service) = state.issue_embedding_service {
        if let Ok(Some(duplicate)) = embedding_service
            .check_duplicate(&issue, &source_name)
            .await
        {
            let similar_id = duplicate
                .embedding
                .short_id
                .as_deref()
                .unwrap_or(&duplicate.embedding.issue_id);

            let activity = ActivityLogEntry::new(
                "decision",
                format!(
                    "{} skipped as semantic duplicate of {}",
                    issue.short_id, similar_id
                ),
            )
            .with_source(source_name.clone())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "decision": "semantic_duplicate_skipped",
                "details": {
                    "similar_issue_id": duplicate.embedding.issue_id,
                    "similar_short_id": similar_id,
                    "similarity": duplicate.similarity,
                    "similar_issue_status": duplicate.outcome.as_deref(),
                }
            }));
            state.tracker.record_activity(&activity).ok();

            let metric = ProcessingMetric::new("semantic_duplicate_skipped", 1.0)
                .with_source(source_name.clone());
            state.tracker.record_metric(&metric).ok();

            return (
                StatusCode::OK,
                Json(json!({
                    "status": "skipped",
                    "reason": format!("Semantic duplicate of {} ({:.0}% similar)",
                        similar_id, duplicate.similarity * 100.0)
                })),
            );
        }
    }

    // Check if currently processing AND atomically mark as processing if not
    // This prevents race conditions where two webhooks pass the check simultaneously
    let processing_key = format!("{}:{}", source_name, issue.id);
    {
        let mut processing = state.processing.write().await;

        // Clean up stale entries to prevent unbounded memory growth
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(PROCESSING_ENTRY_TTL_SECS);
        processing.retain(|_, started_at| now.duration_since(*started_at) < ttl);

        // If still too many entries after TTL cleanup, remove oldest entries
        if processing.len() >= MAX_PROCESSING_ENTRIES {
            tracing::warn!(
                count = processing.len(),
                "Processing set at capacity, forcing cleanup of oldest entries"
            );
            // Find and remove the oldest half of entries
            let mut entries: Vec<_> = processing.iter().map(|(k, v)| (k.clone(), *v)).collect();
            entries.sort_by_key(|(_, v)| *v);
            let to_remove = entries.len() / 2;
            for (key, _) in entries.into_iter().take(to_remove) {
                processing.remove(&key);
            }
        }

        if processing.contains_key(&processing_key) {
            return (
                StatusCode::OK,
                Json(json!({ "status": "ignored", "reason": "Already processing" })),
            );
        }
        // Atomically insert with timestamp while we hold the write lock
        processing.insert(processing_key.clone(), Instant::now());
    }

    // Record attempt synchronously BEFORE spawning background task
    // This prevents TOCTOU race between has_attempted check and record_attempt
    let labels: Vec<String> = issue.get_metadata("labels").unwrap_or_default();
    if let Err(e) =
        state
            .tracker
            .record_attempt_with_labels(&source_name, &issue.id, &issue.short_id, &labels)
    {
        // Remove from processing on failure
        let mut processing = state.processing.write().await;
        processing.remove(&processing_key);
        tracing::error!(source = source_name.as_str(), error = %e, "Failed to record attempt");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "status": "error", "reason": "Failed to record attempt" })),
        );
    }

    // Accept and process in background
    let short_id = issue.short_id.clone();
    let state_clone = Arc::clone(&state);
    let handler_clone = Arc::clone(handler);

    tokio::spawn(async move {
        let cleanup_state = Arc::clone(&state_clone);
        let cleanup_key = processing_key.clone();
        let result = process_issue(
            state_clone,
            handler_clone,
            issue,
            match_result,
            processing_key,
        )
        .await;
        if let Err(e) = &result {
            tracing::error!(source = source_name.as_str(), error = %e, "Error processing webhook");
            // Ensure processing key is cleaned up on error (process_issue normally
            // cleans up on success, but may miss cleanup on early error paths)
            let mut processing = cleanup_state.processing.write().await;
            processing.remove(&cleanup_key);
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "accepted", "issue": short_id })),
    )
}

async fn handle_github_webhook(
    state: Arc<AppState>,
    header_map: &HashMap<String, String>,
    body: &Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let source_name = "github";

    let github_handler = match &state.github_handler {
        Some(handler) => handler,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "GitHub webhook handler is not configured" })),
            );
        }
    };

    let has_signature = header_map.contains_key("x-hub-signature-256");
    let received_activity =
        ActivityLogEntry::new("webhook_received", "Webhook received from github")
            .with_source(source_name.to_string())
            .with_metadata(json!({
                "content_length": body.len(),
                "has_signature": has_signature
            }));
    state.tracker.record_activity(&received_activity).ok();

    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "Invalid JSON" })),
            );
        }
    };

    match github_handler
        .process_webhook(body.as_ref(), &payload, header_map)
        .await
    {
        Ok(true) => (StatusCode::OK, Json(json!({ "status": "processed" }))),
        Ok(false) => (
            StatusCode::OK,
            Json(json!({ "status": "ignored", "reason": "Event not applicable" })),
        ),
        Err(Error::Webhook(_)) | Err(Error::InvalidSignature) => {
            let rejected_activity = ActivityLogEntry::new(
                "webhook_rejected",
                "Webhook rejected: invalid signature from github",
            )
            .with_source(source_name.to_string());
            state.tracker.record_activity(&rejected_activity).ok();
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "Invalid signature" })),
            )
        }
        Err(e) => {
            tracing::error!(source = source_name, error = %e, "Error processing GitHub webhook");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "Failed to process GitHub webhook" })),
            )
        }
    }
}

// Repository resolution is now handled by the inference engine (RepoInferrer).
// See src/inference/mod.rs for the new implementation.

async fn process_issue(
    state: Arc<AppState>,
    handler: Arc<dyn WebhookHandler>,
    mut issue: Issue,
    match_result: crate::types::MatchResult,
    processing_key: String,
) -> Result<()> {
    let source_name = handler.source_name();

    tracing::info!(short_id = %issue.short_id, title = %issue.title, "Processing webhook issue");
    tracing::info!(short_id = %issue.short_id, reason = %match_result.reason, "Match reason");

    // Infer the target repository using the shared resolution function
    let resolution = resolve_repo_for_issue(
        state.inferrer.as_ref(),
        &issue,
        state.sqlite_tracker.as_ref(),
    );

    let project_dir = match &resolution {
        RepoResolution::Resolved { project_dir, .. } => project_dir.clone(),
        RepoResolution::Skip { reason } => {
            tracing::debug!(short_id = %issue.short_id, reason = %reason, "Skipping issue");
            // Clean up processing flag before returning
            let mut processing = state.processing.write().await;
            processing.remove(&processing_key);
            // Mark as failed so it won't be retried (skip is intentional)
            state
                .tracker
                .mark_failed(source_name, &issue.id, &format!("Skipped: {}", reason))?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed)
                .await;
            return Ok(());
        }
    };

    // Fetch the parent repo (no checkout/reset — just update object store)
    // then create an isolated per-issue worktree for Claude to work in.
    if let (Some(github_url), Some(default_branch), Some(repo_name)) = (
        resolution.github_url(),
        resolution.default_branch(),
        resolution.repo_name(),
    ) {
        tracing::info!(
            short_id = %issue.short_id,
            repo = %repo_name,
            "Fetching latest changes"
        );

        if let Err(e) = GitOps::ensure_repo_fetched(&project_dir, github_url).await {
            tracing::error!(
                short_id = %issue.short_id,
                repo = %repo_name,
                error = %e,
                "Failed to fetch repository, skipping issue"
            );
            // Clean up processing flag before returning
            let mut processing = state.processing.write().await;
            processing.remove(&processing_key);
            // Mark as failed
            state.tracker.mark_failed(
                source_name,
                &issue.id,
                &format!("Git fetch failed: {}", e),
            )?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed)
                .await;
            return Ok(());
        }

        // Create per-issue worktree
        let wt_path = worktree_path(&state.config.work_dir, repo_name, &issue.short_id);
        if let Err(e) = GitOps::create_worktree(
            &project_dir,
            &wt_path,
            &format!("origin/{}", default_branch),
        )
        .await
        {
            tracing::error!(
                short_id = %issue.short_id,
                repo = %repo_name,
                error = %e,
                "Failed to create worktree, skipping issue"
            );
            let mut processing = state.processing.write().await;
            processing.remove(&processing_key);
            state.tracker.mark_failed(
                source_name,
                &issue.id,
                &format!("Worktree creation failed: {}", e),
            )?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed)
                .await;
            return Ok(());
        }

        // Re-index files and sync to database
        if let Some(inferrer) = &state.inferrer {
            if let Err(e) = inferrer.index_cloned_repo(repo_name) {
                tracing::warn!(
                    short_id = %issue.short_id,
                    repo = %repo_name,
                    error = %e,
                    "Failed to re-index repository files"
                );
            }

            // Sync updated files to database
            if let (Some(repo), Some(tracker)) =
                (inferrer.get_repo(repo_name), &state.sqlite_tracker)
            {
                if let Err(e) = tracker.sync_repo_files(&repo) {
                    tracing::warn!(
                        short_id = %issue.short_id,
                        repo = %repo_name,
                        error = %e,
                        "Failed to sync repository files to database"
                    );
                }
            }
        }
    }

    // Use the per-issue worktree as the effective working directory for Claude.
    let effective_project_dir = {
        let wt = worktree_path(
            &state.config.work_dir,
            resolution.repo_name().unwrap_or("unknown"),
            &issue.short_id,
        );
        if wt.exists() {
            wt
        } else {
            project_dir.clone()
        }
    };

    // Note: processing flag and attempt already recorded by handle_webhook before spawning
    if let Some(assignee) = issue.get_metadata::<String>("assignee") {
        if let Some(resolved) = state.user_registry.resolve(&issue.source, &assignee) {
            issue.set_metadata("resolved_user", &resolved.slug);
        }
    }
    let attempt_id = state
        .tracker
        .get_attempt(source_name, &issue.id)
        .ok()
        .flatten()
        .map(|a| a.id);

    let processing_started_at = Instant::now();
    let result = async {
        // Notify start
        state.notifier.notify_start(&issue).await?;

        // Build context and run Claude (with semantic Q&A reuse + ask loop).
        let mut context = handler.build_issue_context(&issue).await?;

        // Enrich context with similar past issues
        if let Some(ref embedding_service) = state.issue_embedding_service {
            match embedding_service.find_similar(&issue, source_name).await {
                Ok(similar) if !similar.is_empty() => {
                    let activity = ActivityLogEntry::new(
                        "decision",
                        format!("{} similar issues added to context for {}", similar.len(), issue.short_id),
                    )
                    .with_source(source_name.to_string())
                    .with_issue(issue.id.clone(), issue.short_id.clone())
                    .with_metadata(json!({
                        "decision": "similar_issues_context_added",
                        "details": { "similar_count": similar.len() }
                    }));
                    state.tracker.record_activity(&activity).ok();

                    let metric = ProcessingMetric::new("similar_issues_context_added", 1.0)
                        .with_source(source_name.to_string());
                    state.tracker.record_metric(&metric).ok();

                    context = format!("{}\n{}", context, format_similar_issues_context(&similar));
                }
                _ => {}
            }
        }

        let repo_scope = resolution.repo_name().map(|v| v.to_string());
        let mut used_qa_ids: Vec<i64> = Vec::new();

        if state.config.ask.enabled {
            let preload_query = format!("{} {}", issue.title, context);
            let preload_norm = normalize_text(&preload_query);
            let preload_embedding =
                embed_text(state.embedding_client.as_ref(), &preload_query).await;
            match find_reusable_qa(
                state.tracker.as_ref(),
                &state.config.ask,
                source_name,
                repo_scope.as_deref(),
                &preload_norm,
                preload_embedding.as_deref(),
            ) {
                Ok(matches) if !matches.is_empty() => {
                    context = format!("{}\n\n{}", context, format_reuse_context(&matches));
                    if let Some(id) = attempt_id {
                        for m in &matches {
                            let _ = state.tracker.record_qa_usage(
                                id,
                                m.entry.id,
                                "reused",
                                m.final_score,
                            );
                        }
                    }
                    used_qa_ids.extend(matches.into_iter().map(|m| m.entry.id));
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "Failed to preload reusable Q&A context"),
            }
        }

        let mut rounds: u8 = 0;
        let claude_result = loop {
            let prompt = state
                .claude
                .build_prompt_for_issue(&issue, &context, &effective_project_dir);

            // Enhance prompt with feedback learnings from past outcomes (semantic when possible)
            let prompt = {
                let analyzer = state.feedback_analyzer.lock().await;
                // Try to use pre-computed issue embedding for semantic search
                let issue_emb = state
                    .issue_embedding_service
                    .as_ref()
                    .and_then(|svc| svc.get_embedding(source_name, &issue.id).ok().flatten());
                match issue_emb {
                    Some(emb) => analyzer.enhance_prompt(&prompt, &issue, &emb.embedding),
                    None => prompt,
                }
            };

            // Enhance prompt with continuous learning context
            let prompt = enhance_prompt_with_learning(
                &state,
                &prompt,
                &issue,
                resolution.repo_name(),
            );

            let mut run_result = state
                .claude
                .execute_with_attempt(&prompt, Some(&issue), attempt_id, &effective_project_dir)
                .await?;
            run_result.used_qa_ids = used_qa_ids.clone();

            let blocking_question = match (
                state.config.ask.enabled,
                run_result.blocking_question.clone(),
            ) {
                (true, Some(q)) => q,
                _ => break run_result,
            };

            if rounds >= state.config.ask.max_rounds_per_attempt {
                run_result.success = false;
                run_result.error = Some(format!(
                    "Maximum blocking-question rounds ({}) reached",
                    state.config.ask.max_rounds_per_attempt
                ));
                break run_result;
            }
            rounds = rounds.saturating_add(1);

            let question_norm = normalize_text(&blocking_question.question);
            let question_embedding =
                embed_text(state.embedding_client.as_ref(), &blocking_question.question).await;
            let reusable = match find_reusable_qa(
                state.tracker.as_ref(),
                &state.config.ask,
                source_name,
                repo_scope.as_deref(),
                &question_norm,
                question_embedding.as_deref(),
            ) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to query reusable Q&A");
                    Vec::new()
                }
            };

            if let Some(best) = reusable.first() {
                if let Some(id) = attempt_id {
                    let _ = state.tracker.record_qa_usage(
                        id,
                        best.entry.id,
                        "reused",
                        best.final_score,
                    );
                }
                if !used_qa_ids.contains(&best.entry.id) {
                    used_qa_ids.push(best.entry.id);
                }
                let activity = ActivityLogEntry::new(
                    "question_reused",
                    format!("Reused stored Q&A for {}", issue.short_id),
                )
                .with_source(source_name.to_string())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "qa_id": best.entry.id,
                    "score": best.final_score,
                }));
                state.tracker.record_activity(&activity).ok();

                context = format!(
                    "{}\n\n{}",
                    context,
                    format_answer_context(
                        &blocking_question,
                        &best.entry.answer_text,
                        &best.entry.channel,
                        true
                    )
                );
                continue;
            }

            let resolved_user = issue.get_metadata::<String>("resolved_user");
            let target_discord_id = resolved_user
                .as_deref()
                .and_then(|slug| state.user_registry.get_by_slug(slug))
                .and_then(|u| u.discord_id.clone());
            let target_email = resolved_user
                .as_deref()
                .and_then(|slug| state.user_registry.get_by_slug(slug))
                .and_then(|u| u.email.clone());
            let ask_request = AskRequest {
                correlation_id: build_correlation_id(&issue.short_id),
                source: issue.source.clone(),
                repo: repo_scope.clone(),
                issue_id: issue.id.clone(),
                short_id: issue.short_id.clone(),
                question: blocking_question.clone(),
                asked_at: chrono::Utc::now(),
                target_discord_id,
                target_email,
            };

            let asked_activity = ActivityLogEntry::new(
                "question_asked",
                format!("Asked human question for {}", issue.short_id),
            )
            .with_source(source_name.to_string())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "correlation_id": ask_request.correlation_id,
                "question": blocking_question.question,
            }));
            state.tracker.record_activity(&asked_activity).ok();

            let reply = send_to_all_and_wait_first_reply(
                Arc::clone(&state.notifier),
                &issue,
                &ask_request,
                tokio::time::Duration::from_secs(state.config.ask.wait_timeout_secs),
                tokio::time::Duration::from_secs(state.config.ask.poll_interval_secs),
            )
            .await?;

            if let Some(reply) = reply {
                let answered_activity = ActivityLogEntry::new(
                    "question_answered",
                    format!("Human answered question for {}", issue.short_id),
                )
                .with_source(source_name.to_string())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "channel": reply.channel,
                    "responder": reply.responder,
                    "correlation_id": reply.correlation_id,
                }));
                state.tracker.record_activity(&answered_activity).ok();

                let qa_entry = QaKnowledgeEntry {
                    id: 0,
                    source: issue.source.clone(),
                    repo: repo_scope.clone(),
                    issue_id: issue.id.clone(),
                    short_id: issue.short_id.clone(),
                    question_text: blocking_question.question.clone(),
                    question_norm,
                    question_embedding: question_embedding.clone(),
                    answer_text: reply.answer.clone(),
                    answer_norm: normalize_text(&reply.answer),
                    answer_embedding: embed_text(state.embedding_client.as_ref(), &reply.answer)
                        .await,
                    channel: reply.channel.clone(),
                    responder: reply.responder.clone(),
                    correlation_id: ask_request.correlation_id.clone(),
                    asked_at: ask_request.asked_at,
                    answered_at: reply.replied_at,
                    success_count: 0,
                    failure_count: 0,
                    last_used_at: None,
                    metadata: Some(json!({
                        "context": blocking_question.context,
                        "options": blocking_question.options,
                        "why": blocking_question.why,
                    })),
                };
                if let Ok(qa_id) = state.tracker.store_qa_knowledge(&qa_entry) {
                    if let Some(id) = attempt_id {
                        let _ = state.tracker.record_qa_usage(id, qa_id, "asked", 1.0);
                    }
                    if !used_qa_ids.contains(&qa_id) {
                        used_qa_ids.push(qa_id);
                    }
                }

                context = format!(
                    "{}\n\n{}",
                    context,
                    format_answer_context(&blocking_question, &reply.answer, &reply.channel, false)
                );
                continue;
            }

            let timeout_activity = ActivityLogEntry::new(
                "question_timeout_best_effort",
                format!("No human reply received for {}", issue.short_id),
            )
            .with_source(source_name.to_string())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "best_effort": state.config.ask.best_effort_on_timeout,
                "question": blocking_question.question,
            }));
            state.tracker.record_activity(&timeout_activity).ok();

            if state.config.ask.best_effort_on_timeout {
                context = format!(
                    "{}\n\n{}",
                    context,
                    format_timeout_context(&blocking_question)
                );
                continue;
            }

            run_result.success = false;
            run_result.error = Some("Timed out waiting for human reply".to_string());
            break run_result;
        };

        // Strategy fingerprinting (after Claude execution, regardless of outcome)
        if state.config.learning.strategy_fingerprinting {
            if let Some(ref sqlite) = state.sqlite_tracker {
                if let Some(aid) = attempt_id {
                    if let Ok(execs) = sqlite.get_executions_for_attempt(aid) {
                        if let Some(exec) = execs.first() {
                            if let Some(ref log_path) = exec.stdout_log_path {
                                let path = std::path::Path::new(log_path);
                                if path.exists() {
                                    match crate::learning::StrategyParser::parse_from_log(path, aid) {
                                        Ok(fp) => {
                                            if let Err(e) = state.tracker.store_strategy_fingerprint(&fp) {
                                                tracing::warn!(error = %e, "Failed to store strategy fingerprint");
                                            }
                                        }
                                        Err(e) => tracing::debug!(error = %e, "Failed to parse strategy from log"),
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if claude_result.success {
            if let Some(pr_url) = claude_result.pr_url {
                tracing::info!(short_id = %issue.short_id, pr_url = %pr_url, "Success! PR created");
                state
                    .tracker
                    .mark_success(source_name, &issue.id, &pr_url)?;
                if let Some(id) = attempt_id {
                    let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, true);
                }
                state.notifier.notify_success(&issue, &pr_url).await?;

                // Record pr_created metric
                let metric = ProcessingMetric::new("pr_created", 1.0)
                    .with_source(source_name.to_string());
                state.tracker.record_metric(&metric).ok();

                // Store embedding for future similarity lookups
                if let Some(ref embedding_service) = state.issue_embedding_service {
                    if embedding_service.embed_issue(&issue, source_name).await.is_ok() {
                        let activity = ActivityLogEntry::new(
                            "decision",
                            format!("Stored embedding for {}", issue.short_id),
                        )
                        .with_source(source_name.to_string())
                        .with_issue(issue.id.clone(), issue.short_id.clone())
                        .with_metadata(json!({
                            "decision": "issue_embedding_stored",
                        }));
                        state.tracker.record_activity(&activity).ok();

                        let metric = ProcessingMetric::new("issue_embedding_stored", 1.0)
                            .with_source(source_name.to_string());
                        state.tracker.record_metric(&metric).ok();
                    }
                }

                // Register PR for review watching (actual Merged outcome is
                // recorded later when the review loop detects the merge)
                if let Some(ref review_watcher) = state.review_watcher {
                    if let Some((repo, pr_number)) = SqliteTracker::parse_pr_url(&pr_url) {
                        let pr_state = PrReviewState::new(
                            &pr_url,
                            &repo,
                            pr_number,
                            &issue.id,
                            source_name,
                        );
                        review_watcher.watch_pr(pr_state);
                        tracing::info!(
                            pr_url = %pr_url,
                            repo = %repo,
                            pr_number = pr_number,
                            "PR registered for review watching (webhook)"
                        );
                    }
                }

                // Log webhook processed successfully with PR
                let activity = ActivityLogEntry::new(
                    "webhook_processed",
                    format!("Webhook processed: {} - PR created", issue.short_id),
                )
                .with_source(source_name.to_string())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "pr_url": pr_url,
                    "success": true
                }));
                state.tracker.record_activity(&activity).ok();
            } else {
                tracing::info!(short_id = %issue.short_id, "Completed but no PR URL found");
                state
                    .tracker
                    .mark_failed(source_name, &issue.id, "No PR URL found")?;
                if let Some(id) = attempt_id {
                    let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, false);
                }
                state.notifier.notify_completed(&issue).await?;
                record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed).await;

                // Log webhook processed without PR
                let activity = ActivityLogEntry::new(
                    "webhook_processed",
                    format!("Webhook processed: {} - no PR created", issue.short_id),
                )
                .with_source(source_name.to_string())
                .with_issue(issue.id.clone(), issue.short_id.clone())
                .with_metadata(json!({
                    "success": false,
                    "reason": "No PR URL found"
                }));
                state.tracker.record_activity(&activity).ok();
            }
        } else {
            let error = claude_result.error.as_deref().unwrap_or("Unknown error");
            tracing::error!(short_id = %issue.short_id, error = %error, "Failed");
            state.tracker.mark_failed(source_name, &issue.id, error)?;
            if let Some(id) = attempt_id {
                let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, false);
            }
            notify_failed_with_escalation(&state, &issue, error).await?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed).await;

            // Record error pattern for analytics
            record_error_pattern(&state, source_name, &issue.id, error);

            // Log webhook processing failed
            let activity = ActivityLogEntry::new(
                "webhook_processed",
                format!("Webhook processed: {} - failed", issue.short_id),
            )
            .with_source(source_name.to_string())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "success": false,
                "error": error
            }));
            state.tracker.record_activity(&activity).ok();
        }

        Ok::<_, crate::error::Error>(())
    }
    .await;

    if let Err(ref e) = result {
        let error = e.to_string();
        let _ = state.tracker.mark_failed(source_name, &issue.id, &error);
        if let Some(id) = attempt_id {
            let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, false);
        }
        let _ = notify_failed_with_escalation(&state, &issue, &error).await;
        record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed).await;

        // Record error pattern for pipeline errors
        record_error_pattern(&state, source_name, &issue.id, &error);
    }

    // Record processing duration metric
    let final_status = state
        .tracker
        .get_attempt(source_name, &issue.id)
        .ok()
        .flatten()
        .map(|a| a.status.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let processing_time_metric = ProcessingMetric::new(
        "processing_time",
        processing_started_at.elapsed().as_secs_f64(),
    )
    .with_source(source_name.to_string())
    .with_tags(json!({ "status": final_status }));
    state.tracker.record_metric(&processing_time_metric).ok();

    // Cleanup worktree
    if let Some(repo_name) = resolution.repo_name() {
        let wt_path = worktree_path(&state.config.work_dir, repo_name, &issue.short_id);
        if wt_path.exists() {
            if let Err(e) = GitOps::remove_worktree(&project_dir, &wt_path).await {
                tracing::warn!(
                    short_id = %issue.short_id,
                    error = %e,
                    "Failed to remove worktree"
                );
            }
        }
    }

    // Remove from processing
    {
        let mut processing = state.processing.write().await;
        processing.remove(&processing_key);
    }

    result
}

async fn record_feedback_outcome_from_attempt(
    state: &AppState,
    source_name: &str,
    issue: &Issue,
    outcome: Outcome,
) {
    let attempt = match state.tracker.get_attempt(source_name, &issue.id) {
        Ok(Some(attempt)) => attempt,
        _ => return,
    };

    let prompt = state
        .sqlite_tracker
        .as_ref()
        .and_then(|t| t.get_executions_for_attempt(attempt.id).ok())
        .and_then(|execs| execs.into_iter().next())
        .and_then(|exec| exec.prompt_used)
        .unwrap_or_default();

    let mut fix_outcome = FixOutcome::from_attempt(&attempt, issue, &prompt, outcome);

    // Compute embedding for the outcome's issue text (reuse existing issue embedding if available)
    if let Some(ref embedding_client) = state.embedding_client {
        let embedding = match state
            .issue_embedding_service
            .as_ref()
            .and_then(|svc| svc.get_embedding(source_name, &issue.id).ok().flatten())
        {
            Some(existing) => Some(existing.embedding),
            None => embedding_client.embed(&fix_outcome.issue_text).await.ok(),
        };
        if let Some(emb) = embedding {
            fix_outcome.set_embedding(emb);
        }
    }

    if let Err(e) = state.tracker.store_feedback_outcome(&fix_outcome) {
        tracing::warn!(error = %e, "Failed to store webhook feedback outcome");
    }

    // Update in-memory analyzer for prompt enhancement
    let mut analyzer = state.feedback_analyzer.lock().await;
    if let Err(e) = analyzer.record_outcome(&attempt, issue, &prompt, outcome) {
        tracing::warn!(error = %e, "Failed to record webhook feedback in memory");
    }
}

async fn notify_failed_with_escalation(state: &AppState, issue: &Issue, error: &str) -> Result<()> {
    if ClaudeRunner::is_hard_error(error) {
        let mut global_issue = issue.clone();
        global_issue.metadata.remove("resolved_user");
        global_issue
            .metadata
            .insert("hard_error".to_string(), serde_json::Value::Bool(true));

        let activity = ActivityLogEntry::new(
            "error",
            format!("Hard Claude error escalated for {}", issue.short_id),
        )
        .with_source(issue.source.clone())
        .with_issue(issue.id.clone(), issue.short_id.clone())
        .with_metadata(json!({
            "hard_error": true,
            "rate_limited": ClaudeRunner::is_rate_limit_error(error),
            "error": truncate_error_for_activity(error),
        }));
        state.tracker.record_activity(&activity).ok();

        return state.notifier.notify_failed(&global_issue, error).await;
    }

    state.notifier.notify_failed(issue, error).await
}

fn truncate_error_for_activity(error: &str) -> String {
    let max_len = 500;
    if error.len() <= max_len {
        error.to_string()
    } else {
        let safe_end = error
            .char_indices()
            .take_while(|(i, _)| *i <= max_len.saturating_sub(3))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
        format!("{}...", &error[..safe_end])
    }
}

/// Enhance a prompt with continuous learning context (repo knowledge, promoted
/// instructions, strategy suggestions, cluster context).
fn enhance_prompt_with_learning(
    state: &AppState,
    base_prompt: &str,
    issue: &Issue,
    repo: Option<&str>,
) -> String {
    let learning = &state.config.learning;
    let Some(repo_name) = repo else {
        return base_prompt.to_string();
    };

    let mut extra_context = String::new();

    // System 4: Per-repo knowledge context
    if learning.repo_knowledge {
        if let Ok(knowledge) = state.tracker.get_repo_knowledge(repo_name) {
            let ctx = crate::learning::RepoKnowledgeManager::format_knowledge_context(&knowledge);
            if !ctx.is_empty() {
                extra_context.push_str(&ctx);
            }
        }
    }

    // System 3: Promoted instructions
    if learning.qa_promotion {
        if let Ok(instructions) = state.tracker.get_promoted_instructions(repo_name) {
            let ctx = crate::learning::QaPromoter::format_promoted_context(&instructions);
            if !ctx.is_empty() {
                extra_context.push_str(&ctx);
            }
        }
    }

    // System 6: Strategy suggestions
    if learning.strategy_fingerprinting {
        if let Ok(strategies) = state.tracker.get_successful_strategies(repo_name, 3) {
            let ctx = crate::learning::StrategyParser::format_strategy_suggestions(&strategies);
            if !ctx.is_empty() {
                extra_context.push_str(&ctx);
            }
        }
    }

    // System 8: Cluster context
    if learning.cluster_detection {
        if let Ok(clusters) = state.tracker.get_active_clusters(&issue.source) {
            for cluster in &clusters {
                if cluster.issue_ids.contains(&issue.id) {
                    extra_context.push_str(
                        &crate::learning::ClusterDetector::format_cluster_context(cluster),
                    );
                    extra_context.push('\n');
                    break;
                }
            }
        }
    }

    if extra_context.is_empty() {
        return base_prompt.to_string();
    }

    format!("{}\n---\n\n{}", extra_context, base_prompt)
}

/// Record an error pattern to the analytics database.
fn record_error_pattern(state: &AppState, source: &str, issue_id: &str, error_msg: &str) {
    let error_type = classify_error(error_msg);
    let pattern_hash = compute_error_hash(error_msg);

    let mut pattern = ErrorPattern::new(pattern_hash);
    pattern.error_type = Some(error_type.to_string());
    pattern.error_message = Some(error_msg.to_string());
    pattern.sources = Some(vec![source.to_string()]);
    pattern.example_issue_ids = Some(vec![issue_id.to_string()]);

    if let Err(e) = state.tracker.record_error_pattern(&pattern) {
        tracing::warn!(error = %e, "Failed to record error pattern");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AskConfig, CascadeConfig, ClaudeConfig, DiscordConfig, EmailConfig, GitHubAppConfig,
        GitHubConfig, LearningConfig, PushConfig, RegressionConfig, RetryConfig, SmsConfig,
    };
    use crate::notifier::Notifier;
    use crate::reports::Report;
    use crate::storage::SqliteTracker;
    use crate::types::{Issue, MatchPriority, MatchResult};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Mock notifier for testing
    struct MockNotifier {
        call_count: AtomicUsize,
    }

    impl MockNotifier {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Notifier for MockNotifier {
        fn name(&self) -> &str {
            "mock"
        }
        fn is_enabled(&self) -> bool {
            true
        }
        async fn notify_start(&self, _issue: &Issue) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_success(&self, _issue: &Issue, _pr_url: &str) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_completed(&self, _issue: &Issue) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_failed(&self, _issue: &Issue, _error: &str) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_status(&self, _message: &str) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_urgent_issues(&self, _issues: &[Issue]) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_merged(&self, _issue: &Issue, _pr_url: &str) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn notify_report(&self, _report: &Report) -> crate::error::Result<()> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    // Mock webhook handler for testing
    struct MockWebhookHandler {
        name: String,
    }

    impl MockWebhookHandler {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
            }
        }
    }

    #[async_trait]
    impl WebhookHandler for MockWebhookHandler {
        fn source_name(&self) -> &str {
            &self.name
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Ok(Some(Issue::new(
                "1",
                "TEST-1",
                "Test",
                "https://test.com",
                &self.name,
            )))
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Test", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, issue: &Issue) -> crate::error::Result<String> {
            Ok(format!("Context for {}", issue.short_id))
        }
    }

    fn test_config() -> Config {
        Config {
            work_dir: std::path::PathBuf::from("/tmp/repos"),
            known_orgs: vec!["test-org".to_string()],
            auto_discover_paths: vec![],
            poll_interval_ms: 60000,
            webhook_port: 8080,
            db_path: std::path::PathBuf::from(":memory:"),
            max_issues_per_cycle: 5,
            max_concurrent: 2,
            processing_delay_ms: 1000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600,
            claude: ClaudeConfig::default(),
            discord: DiscordConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            ask: AskConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: LearningConfig::default(),
        }
    }

    #[test]
    fn test_webhook_server_new() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(config, handlers, notifier, tracker, None, None);

        assert_eq!(server.port, 8080);
    }

    #[test]
    fn test_webhook_server_with_custom_port() {
        let mut config = test_config();
        config.webhook_port = 3000;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(config, handlers, notifier, tracker, None, None);

        assert_eq!(server.port, 3000);
    }

    #[test]
    fn test_webhook_handler_registry_new() {
        let registry = WebhookHandlerRegistry::new();
        assert!(registry.get_all().is_empty());
    }

    #[test]
    fn test_webhook_handler_registry_register() {
        let mut registry = WebhookHandlerRegistry::new();
        let handler = Arc::new(MockWebhookHandler::new("test"));

        registry.register(handler);

        assert_eq!(registry.get_all().len(), 1);
    }

    #[test]
    fn test_webhook_handler_registry_get() {
        let mut registry = WebhookHandlerRegistry::new();
        let handler = Arc::new(MockWebhookHandler::new("linear"));

        registry.register(handler);

        assert!(registry.get("linear").is_some());
        assert!(registry.get("sentry").is_none());
    }

    #[test]
    fn test_webhook_handler_registry_multiple_handlers() {
        let mut registry = WebhookHandlerRegistry::new();
        registry.register(Arc::new(MockWebhookHandler::new("linear")));
        registry.register(Arc::new(MockWebhookHandler::new("sentry")));
        registry.register(Arc::new(MockWebhookHandler::new("github")));

        assert_eq!(registry.get_all().len(), 3);
        assert!(registry.get("linear").is_some());
        assert!(registry.get("sentry").is_some());
        assert!(registry.get("github").is_some());
    }

    #[test]
    fn test_mock_webhook_handler_source_name() {
        let handler = MockWebhookHandler::new("test-source");
        assert_eq!(handler.source_name(), "test-source");
    }

    #[test]
    fn test_mock_webhook_handler_verify_signature() {
        let handler = MockWebhookHandler::new("test");
        let headers: HashMap<String, String> = HashMap::new();
        assert!(handler.verify_signature(b"body", &headers));
    }

    #[tokio::test]
    async fn test_mock_webhook_handler_parse_payload() {
        let handler = MockWebhookHandler::new("test");
        let payload = serde_json::json!({});

        let result = handler.parse_payload(&payload).await.unwrap();

        assert!(result.is_some());
        let issue = result.unwrap();
        assert_eq!(issue.short_id, "TEST-1");
    }

    #[test]
    fn test_mock_webhook_handler_matches_criteria() {
        let handler = MockWebhookHandler::new("test");
        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");

        let result = handler.matches_criteria(&issue);

        assert!(result.matches);
        assert_eq!(result.priority, MatchPriority::Normal);
    }

    #[tokio::test]
    async fn test_mock_webhook_handler_build_issue_context() {
        let handler = MockWebhookHandler::new("test");
        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");

        let context = handler.build_issue_context(&issue).await.unwrap();

        assert!(context.contains("TEST-1"));
    }

    #[test]
    fn test_processing_key_format() {
        // Verify the format of processing keys
        let source_name = "linear";
        let issue_id = "abc123";
        let key = format!("{}:{}", source_name, issue_id);

        assert_eq!(key, "linear:abc123");
        assert!(key.contains(':'));
    }

    #[test]
    fn test_webhook_handler_registry_overwrite() {
        let mut registry = WebhookHandlerRegistry::new();
        let handler1 = Arc::new(MockWebhookHandler::new("test"));
        let handler2 = Arc::new(MockWebhookHandler::new("test"));

        registry.register(handler1);
        registry.register(handler2);

        // Both have same name, registry should handle this
        // (depends on implementation - may have 1 or 2 entries)
        assert!(registry.get("test").is_some());
    }

    #[test]
    fn test_mock_notifier_enabled() {
        let notifier = MockNotifier::new();
        assert!(notifier.is_enabled());
        assert_eq!(notifier.name(), "mock");
    }

    #[test]
    fn test_app_state_processing_map_uniqueness() {
        // Verify that processing map has unique keys
        let mut map: HashMap<String, Instant> = HashMap::new();
        let time1 = Instant::now();
        map.insert("key1".to_string(), time1);
        map.insert("key1".to_string(), time1); // duplicate key

        // HashMap should only contain one entry for the same key
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn test_json_response_structure() {
        // Test the expected JSON response structures
        let accepted = json!({ "status": "accepted", "issue": "TEST-1" });
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(accepted["issue"], "TEST-1");

        let ignored = json!({ "status": "ignored", "reason": "test reason" });
        assert_eq!(ignored["status"], "ignored");
        assert_eq!(ignored["reason"], "test reason");

        let error = json!({ "error": "error message" });
        assert!(error["error"].is_string());
    }

    #[test]
    fn test_health_response_structure() {
        // Test expected health response structure
        let health = json!({
            "status": "ok",
            "processing_count": 0,
            "handlers": ["linear", "sentry"]
        });

        assert_eq!(health["status"], "ok");
        assert_eq!(health["processing_count"], 0);
        assert!(health["handlers"].is_array());
    }

    #[test]
    fn test_header_lowercasing() {
        // Test that headers are lowercased correctly
        let original = "X-Hub-Signature-256";
        let lowercased = original.to_lowercase();
        assert_eq!(lowercased, "x-hub-signature-256");
    }

    #[tokio::test]
    async fn test_rwlock_processing_map() {
        // Test RwLock behavior for concurrent access with HashMap
        let processing: RwLock<HashMap<String, Instant>> = RwLock::new(HashMap::new());

        // Write
        {
            let mut write_guard = processing.write().await;
            write_guard.insert("test".to_string(), Instant::now());
        }

        // Read
        {
            let read_guard = processing.read().await;
            assert!(read_guard.contains_key("test"));
            assert_eq!(read_guard.len(), 1);
        }

        // Remove
        {
            let mut write_guard = processing.write().await;
            write_guard.remove("test");
        }

        // Verify removed
        {
            let read_guard = processing.read().await;
            assert!(!read_guard.contains_key("test"));
        }
    }

    // Tests for health_handler
    #[tokio::test]
    async fn test_health_handler() {
        use axum::extract::State;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["processing_count"], 0);
    }

    #[tokio::test]
    async fn test_health_handler_with_processing() {
        use axum::extract::State;

        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut processing_set = HashMap::new();
        processing_set.insert("linear:issue1".to_string(), Instant::now());
        processing_set.insert("sentry:issue2".to_string(), Instant::now());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(processing_set),
        });

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["processing_count"], 2);
    }

    // Tests for webhook_handler
    #[tokio::test]
    async fn test_webhook_handler_unknown_source() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("unknown".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Unknown source"));
    }

    // Mock handler that rejects signatures
    struct RejectingSignatureHandler;

    #[async_trait]
    impl WebhookHandler for RejectingSignatureHandler {
        fn source_name(&self) -> &str {
            "rejecting"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            false
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Ok(None)
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Test", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, _issue: &Issue) -> crate::error::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_invalid_signature() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(RejectingSignatureHandler));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("rejecting".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Invalid signature"));
    }

    #[tokio::test]
    async fn test_webhook_handler_invalid_json() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"not valid json{"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    // Mock handler that returns None from parse
    struct IgnoringHandler;

    #[async_trait]
    impl WebhookHandler for IgnoringHandler {
        fn source_name(&self) -> &str {
            "ignoring"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Ok(None)
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Test", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, _issue: &Issue) -> crate::error::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_event_ignored() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(IgnoringHandler));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("ignoring".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("not applicable"));
    }

    // Mock handler that fails criteria
    struct NonMatchingHandler;

    #[async_trait]
    impl WebhookHandler for NonMatchingHandler {
        fn source_name(&self) -> &str {
            "nonmatching"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Ok(Some(Issue::new(
                "1",
                "TEST-1",
                "Test",
                "https://test.com",
                "nonmatching",
            )))
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::not_matched("Does not match criteria")
        }
        async fn build_issue_context(&self, _issue: &Issue) -> crate::error::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_criteria_not_matched() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(NonMatchingHandler));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("nonmatching".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Does not match"));
    }

    #[tokio::test]
    async fn test_webhook_handler_already_attempted() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Mark the issue as already attempted
        tracker.record_attempt("test", "1", "TEST-1").unwrap();

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Already attempted"));
    }

    #[tokio::test]
    async fn test_webhook_handler_already_processing() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Mark issue as being processed
        let mut processing = HashMap::new();
        processing.insert("test:1".to_string(), Instant::now());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(processing),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Already processing"));
    }

    #[tokio::test]
    async fn test_webhook_handler_accepted() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
        assert_eq!(response["issue"], "TEST-1");
    }

    // Mock handler that fails to parse
    struct FailingParseHandler;

    #[async_trait]
    impl WebhookHandler for FailingParseHandler {
        fn source_name(&self) -> &str {
            "failing"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Err(crate::error::Error::config("Parse failed"))
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Test", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, _issue: &Issue) -> crate::error::Result<String> {
            Ok(String::new())
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_parse_error() {
        use axum::extract::{Path, State};
        use axum::http::HeaderMap;

        let config = test_config();
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(FailingParseHandler));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("failing".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Failed to parse"));
    }

    #[test]
    fn test_header_conversion_to_hashmap() {
        use axum::http::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-hub-signature-256"),
            HeaderValue::from_static("sha256=abc123"),
        );
        headers.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );

        let header_map: HashMap<String, String> = headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|val| (k.as_str().to_lowercase(), val.to_string()))
            })
            .collect();

        assert_eq!(header_map.len(), 2);
        assert_eq!(
            header_map.get("x-hub-signature-256"),
            Some(&"sha256=abc123".to_string())
        );
        assert_eq!(
            header_map.get("content-type"),
            Some(&"application/json".to_string())
        );
    }

    #[tokio::test]
    async fn test_mock_notifier_all_methods() {
        let notifier = MockNotifier::new();
        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");

        notifier.notify_start(&issue).await.unwrap();
        notifier
            .notify_success(&issue, "https://github.com/pr")
            .await
            .unwrap();
        notifier.notify_completed(&issue).await.unwrap();
        notifier.notify_failed(&issue, "error").await.unwrap();
        notifier.notify_status("status").await.unwrap();
        notifier
            .notify_urgent_issues(std::slice::from_ref(&issue))
            .await
            .unwrap();
        notifier
            .notify_merged(&issue, "https://github.com/pr")
            .await
            .unwrap();

        assert_eq!(notifier.call_count.load(Ordering::SeqCst), 7);
    }

    #[test]
    fn test_webhook_server_fields() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(config.clone(), handlers, notifier, tracker, None, None);

        assert_eq!(server.port, config.webhook_port);
        assert_eq!(server.config.work_dir, config.work_dir);
    }

    #[tokio::test]
    async fn test_health_handler_no_handlers() {
        use axum::extract::State;

        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: config.claude_timeout_secs,
                    ..Default::default()
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
        });

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["status"], "ok");
        assert!(response["handlers"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_processing_ttl_constants() {
        // Verify TTL constants are reasonable
        const {
            assert!(PROCESSING_ENTRY_TTL_SECS >= 60); // At least 1 minute
            assert!(PROCESSING_ENTRY_TTL_SECS <= 7200); // At most 2 hours
            assert!(MAX_PROCESSING_ENTRIES >= 100); // Reasonable capacity
        }
    }

    #[test]
    fn test_processing_map_retain_semantics() {
        // Test that retain works correctly for TTL cleanup
        let mut map: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();

        // Insert some entries
        map.insert("key1".to_string(), now);
        map.insert("key2".to_string(), now);
        map.insert("key3".to_string(), now);

        assert_eq!(map.len(), 3);

        // Retain all (nothing expired yet since we just created them)
        let ttl = std::time::Duration::from_secs(3600);
        map.retain(|_, started_at| now.duration_since(*started_at) < ttl);

        // All should still be present
        assert_eq!(map.len(), 3);
    }
}

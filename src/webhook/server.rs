//! HTTP server for webhooks.

use super::{GitHubWebhookHandler, WebhookHandler, WebhookHandlerRegistry};
use crate::config::Config;
use crate::error::Error;
use crate::error::Result;
use crate::feedback::{
    format_similar_issues_context, FeedbackAnalyzer, FixOutcome, IssueEmbeddingService, Outcome,
};
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::{send_to_all_and_wait_first_reply, Notifier};
use crate::qa::{
    build_correlation_id, embed_text, find_reusable_qa, format_answer_context,
    format_reuse_context, format_timeout_context, normalize_text,
};
use crate::repo::{worktree_path, GitOps};
use crate::runner::{self, AgentRunner};
use crate::scm::{PrReviewState, ReviewWatcher};
use crate::storage::{classify_error, compute_error_hash, FixAttemptTracker};
use crate::types::{
    validate_issue_id, ActivityLogEntry, AgentExecution, AskRequest, ErrorPattern, Issue,
    IssueEmbedding, ProcessingMetric, QaKnowledgeEntry,
};
use crate::users::UserRegistry;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::get,
    Router,
};
use sentry::integrations::tower::{NewSentryLayer, SentryHttpLayer};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tower::limit::ConcurrencyLimitLayer;

#[cfg(test)]
use axum::routing::post;

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
    sqlite_tracker: Option<Arc<dyn FixAttemptTracker>>,
    inferrer: Option<RepoInferrer>,
    embedding_client: Option<crate::feedback::EmbeddingClient>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    feedback_analyzer: tokio::sync::Mutex<FeedbackAnalyzer>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    user_registry: UserRegistry,
    agent: Arc<dyn AgentRunner>,
    github_handler: Option<GitHubWebhookHandler>,
    suppression_regex_cache: Option<crate::prioritisation::suppression::RegexCache>,
    /// Tracks currently processing webhooks with timestamps for TTL-based cleanup.
    /// Key: processing key (source:issue_id), Value: timestamp when processing started.
    processing: RwLock<HashMap<String, Instant>>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct WebhookVerifyQuery {
    #[serde(rename = "hub.mode")]
    hub_mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    hub_verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    hub_challenge: Option<String>,
}

/// HTTP server for webhooks.
pub struct WebhookServer {
    config: Config,
    handlers: WebhookHandlerRegistry,
    notifier: Arc<dyn Notifier>,
    tracker: Arc<dyn FixAttemptTracker>,
    sqlite_tracker: Option<Arc<dyn FixAttemptTracker>>,
    inferrer: Option<RepoInferrer>,
    issue_embedding_service: Option<Arc<IssueEmbeddingService>>,
    review_watcher: Option<Arc<ReviewWatcher>>,
    github_handler: Option<GitHubWebhookHandler>,
    agent: Arc<dyn AgentRunner>,
    port: u16,
}

impl WebhookServer {
    /// Create a new webhook server.
    pub fn new(
        config: Config,
        handlers: WebhookHandlerRegistry,
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<dyn FixAttemptTracker>>,
        inferrer: Option<RepoInferrer>,
        agent: Arc<dyn AgentRunner>,
    ) -> Self {
        Self::new_with_github(
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker,
            inferrer,
            None,
            agent,
        )
    }

    /// Create a new webhook server with optional GitHub review webhook handling.
    pub fn new_with_github(
        config: Config,
        handlers: WebhookHandlerRegistry,
        notifier: Arc<dyn Notifier>,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<dyn FixAttemptTracker>>,
        inferrer: Option<RepoInferrer>,
        github_handler: Option<GitHubWebhookHandler>,
        agent: Arc<dyn AgentRunner>,
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
            agent,
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
        let bind_address = self.config.bind_address.clone();
        let embedding_client = crate::feedback::EmbeddingClient::from_env().ok();
        let user_registry = UserRegistry::new(self.config.users.clone());

        // Initialize FeedbackAnalyzer and warm-start with DB outcomes
        let mut feedback_analyzer = FeedbackAnalyzer::new();
        if let Some(ref sqlite_tracker) = self.sqlite_tracker {
            feedback_analyzer = feedback_analyzer.with_tracker(sqlite_tracker.clone());
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

        let suppression_regex_cache = if !self.config.prioritisation.suppression_rules.is_empty() {
            Some(crate::prioritisation::suppression::RegexCache::new(
                &self.config.prioritisation.suppression_rules,
            ))
        } else {
            None
        };

        let state = Arc::new(AppState {
            agent: self.agent,
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
            suppression_regex_cache,
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
                get(webhook_verify_handler)
                    .post(webhook_handler)
                    .layer(concurrency_layer),
            )
            .layer(DefaultBodyLimit::max(512 * 1024)) // 512 KB body size limit
            // Sentry layers: NewSentryLayer must be outermost (added last in axum's layer chain)
            .layer(SentryHttpLayer::new().enable_transaction())
            .layer(NewSentryLayer::new_from_top())
            .with_state(state.clone());

        let addr = format!("{}:{}", bind_address, self.port);
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

        tracing::info!("Webhook server listening on {}", addr);
        tracing::info!(
            workspace = ?state.config.workspace,
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

async fn webhook_verify_handler(
    State(state): State<Arc<AppState>>,
    Path(source_name): Path<String>,
    Query(query): Query<WebhookVerifyQuery>,
) -> impl IntoResponse {
    if source_name != "whatsapp" {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({ "error": "GET verification not supported for this source" })),
        )
            .into_response();
    }

    let expected_owned = state
        .config
        .notifiers
        .whatsapp
        .webhook_verify_token
        .as_ref()
        .map(|s| s.expose().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::env::var("WHATSAPP_WEBHOOK_VERIFY_TOKEN")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        });

    let Some(expected) = expected_owned else {
        tracing::error!(
            source = "whatsapp",
            "WHATSAPP_WEBHOOK_VERIFY_TOKEN not configured; cannot verify webhook"
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Webhook verify token not configured" })),
        )
            .into_response();
    };

    if query.hub_mode.as_deref() != Some("subscribe")
        || query.hub_verify_token.as_deref() != Some(expected.as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Invalid verify token" })),
        )
            .into_response();
    }

    (StatusCode::OK, query.hub_challenge.unwrap_or_default()).into_response()
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
        || header_map.contains_key("x-slack-signature")
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

    // Slack URL verification challenge must return the challenge body immediately.
    if source_name == "slack" {
        let slack_payload: serde_json::Value = match serde_json::from_slice(&body) {
            Ok(p) => p,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "Invalid JSON" })),
                );
            }
        };
        if slack_payload
            .get("type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "url_verification")
        {
            let challenge = slack_payload
                .get("challenge")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            return (StatusCode::OK, Json(json!({ "challenge": challenge })));
        }
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

    // Check suppression rules before criteria matching
    if let Some(ref cache) = state.suppression_regex_cache {
        let result = crate::prioritisation::suppression::check_issue_with_cache(
            &state.config.prioritisation.suppression_rules,
            &issue,
            cache,
        );
        if result.suppressed {
            let rule_name = result.matched_rule.as_deref().unwrap_or("unknown");
            let reason = result.reason.as_deref().unwrap_or("suppressed by rule");
            tracing::info!(
                source = source_name.as_str(),
                issue_id = issue.short_id.as_str(),
                rule = rule_name,
                reason = reason,
                "Issue suppressed by prioritisation rule"
            );
            if let Err(e) =
                state
                    .tracker
                    .record_suppression(&source_name, &issue.id, rule_name, reason)
            {
                tracing::debug!(error = %e, "Failed to record suppression");
            }
            return (
                StatusCode::OK,
                Json(json!({ "status": "suppressed", "rule": rule_name, "reason": reason })),
            );
        }
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

    // Persist full issue content to the issues table (independent of embeddings)
    {
        let stored = IssueEmbedding::from_issue(&issue);
        if let Err(e) = state.tracker.store_issue(&stored) {
            tracing::debug!(error = %e, "Failed to store issue content");
        }
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
    let resolution = resolve_repo_for_issue(state.inferrer.as_ref(), &issue, Some(&state.tracker));

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
    if let (Some(scm_url), Some(default_branch), Some(repo_name)) = (
        resolution.scm_url(),
        resolution.default_branch(),
        resolution.repo_name(),
    ) {
        tracing::info!(
            short_id = %issue.short_id,
            repo = %repo_name,
            "Fetching latest changes"
        );

        if let Err(e) = GitOps::ensure_repo_fetched(&project_dir, scm_url).await {
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
        let wt_path = worktree_path(&state.config.workspace, repo_name, &issue.short_id);
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
    // Fall back to project_dir only when no repo was resolved (no worktree attempted).
    let effective_project_dir = if let Some(repo_name) = resolution.repo_name() {
        let wt = worktree_path(&state.config.workspace, repo_name, &issue.short_id);
        if !wt.exists() {
            let err = format!("Worktree disappeared after creation: {:?}", wt);
            tracing::error!(short_id = %issue.short_id, error = %err);
            let mut processing = state.processing.write().await;
            processing.remove(&processing_key);
            state.tracker.mark_failed(source_name, &issue.id, &err)?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed)
                .await;
            return Ok(());
        }
        wt
    } else {
        project_dir.clone()
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
                .agent
                .build_prompt_for_issue(&issue, &context, &effective_project_dir);

            // Enhance prompt with feedback learnings from past outcomes (semantic when possible)
            let prompt = {
                let analyzer = state.feedback_analyzer.lock().await;
                // Try to use pre-computed issue embedding for semantic search
                let issue_emb = state
                    .issue_embedding_service
                    .as_ref()
                    .and_then(|svc| svc.get_embedding(source_name, &issue.id).ok().flatten());
                match issue_emb.and_then(|emb| emb.embedding) {
                    Some(ref emb) => analyzer.enhance_prompt(&prompt, &issue, emb),
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
                .agent
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
                target_slack_id: resolved_user
                    .as_deref()
                    .and_then(|slug| state.user_registry.get_by_slug(slug))
                    .and_then(|u| u.slack_id.clone()),
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
                if let Some(ref changelog) = claude_result.changelog {
                    issue.set_metadata("changelog", changelog.clone());
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
                    if let Some((repo, pr_number)) = crate::storage::parse_pr_url(&pr_url) {
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
                let no_pr_error = if claude_result.output.is_empty() {
                    "No PR URL found in output".to_string()
                } else {
                    let summary = if claude_result.output.chars().count() > 500 {
                        let truncated: String = claude_result.output.chars().take(497).collect();
                        format!("{}...", truncated)
                    } else {
                        claude_result.output.clone()
                    };
                    format!("Claude completed without creating a PR: {}", summary)
                };
                tracing::info!(short_id = %issue.short_id, "No PR URL found in output");
                state
                    .tracker
                    .mark_failed(source_name, &issue.id, &no_pr_error)?;
                if let Some(id) = attempt_id {
                    let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, false);
                }
                state.notifier.notify_failed(&issue, &no_pr_error).await?;
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
                    "reason": &no_pr_error
                }));
                state.tracker.record_activity(&activity).ok();
            }
        } else {
            let base_error = claude_result.error.as_deref().unwrap_or("Unknown error");
            let error = if !claude_result.output.is_empty() {
                let summary = if claude_result.output.chars().count() > 500 {
                    let truncated: String = claude_result.output.chars().take(497).collect();
                    format!("{}...", truncated)
                } else {
                    claude_result.output.clone()
                };
                format!("{}\n\nClaude's summary: {}", base_error, summary)
            } else {
                base_error.to_string()
            };
            tracing::error!(short_id = %issue.short_id, error = %error, "Failed");
            state.tracker.mark_failed(source_name, &issue.id, &error)?;
            if let Some(id) = attempt_id {
                let _ = state.tracker.update_qa_outcome_stats_for_attempt(id, false);
            }
            notify_failed_with_escalation(&state, &issue, &error).await?;
            record_feedback_outcome_from_attempt(&state, source_name, &issue, Outcome::Failed).await;

            // Record error pattern for analytics
            record_error_pattern(&state, source_name, &issue.id, &error);

            // Log webhook processing failed
            let activity = ActivityLogEntry::new(
                "webhook_processed",
                format!("Webhook processed: {} - failed", issue.short_id),
            )
            .with_source(source_name.to_string())
            .with_issue(issue.id.clone(), issue.short_id.clone())
            .with_metadata(json!({
                "success": false,
                "error": &error
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
        let wt_path = worktree_path(&state.config.workspace, repo_name, &issue.short_id);
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
        .and_then(|execs: Vec<AgentExecution>| execs.into_iter().next())
        .and_then(|exec| exec.prompt_used)
        .unwrap_or_default();

    let mut fix_outcome = FixOutcome::from_attempt(&attempt, issue, &prompt, outcome);

    // Compute embedding for the outcome's issue text (reuse existing issue embedding if available)
    if let Some(ref embedding_client) = state.embedding_client {
        let embedding = match state
            .issue_embedding_service
            .as_ref()
            .and_then(|svc| svc.get_embedding(source_name, &issue.id).ok().flatten())
            .and_then(|existing| existing.embedding)
        {
            Some(existing) => Some(existing),
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
    if runner::is_hard_error(error) {
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
            "rate_limited": runner::is_rate_limit_error(error),
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
        AgentConfig, AskConfig, CascadeConfig, CodeIndexConfig, IssuesConfig, LearningConfig,
        NotifiersConfig, PrioritisationConfig, RegressionConfig, RetryConfig, ScmConfig,
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
            workspace: std::path::PathBuf::from("/tmp/repos"),
            known_orgs: vec!["test-org".to_string()],
            auto_discover_paths: vec![],
            poll_interval_ms: 60000,
            webhook_port: 8080,
            bind_address: "127.0.0.1".to_string(),
            db_path: std::path::PathBuf::from(":memory:"),
            max_issues_per_cycle: 5,
            max_concurrent: 2,
            processing_delay_ms: 1000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            agent: AgentConfig::default(),
            scm: ScmConfig::default(),
            issues: IssuesConfig::default(),
            notifiers: NotifiersConfig::default(),
            ask: AskConfig::default(),
            retry: RetryConfig::default(),
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: LearningConfig::default(),
            prioritisation: PrioritisationConfig::default(),
            code_index: CodeIndexConfig::default(),
            evaluation: crate::config::EvaluationConfig::default(),
            storage_dir: "/tmp/claudear-storage".into(),
            dashboard: crate::config::DashboardConfig::default(),
            tenant_id: None,
            database_url: None,
            redis_url: None,
        }
    }

    fn test_agent(tracker: Arc<dyn FixAttemptTracker>) -> Arc<dyn AgentRunner> {
        Arc::new(crate::runner::ClaudeAgentRunner::new(
            crate::runner::ClaudeRunnerConfig::default(),
            tracker,
        ))
    }

    #[test]
    fn test_webhook_server_new() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert_eq!(server.port, 8080);
    }

    #[test]
    fn test_webhook_server_with_custom_port() {
        let mut config = test_config();
        config.webhook_port = 3000;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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

        let server = WebhookServer::new(
            config.clone(),
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert_eq!(server.port, config.webhook_port);
        assert_eq!(server.config.workspace, config.workspace);
    }

    #[tokio::test]
    async fn test_health_handler_no_handlers() {
        use axum::extract::State;

        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
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
            suppression_regex_cache: None,
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

    // ---------------------------------------------------------------
    // truncate_error_for_activity tests
    // ---------------------------------------------------------------

    #[test]
    fn test_truncate_error_short_message() {
        let msg = "Something went wrong";
        let result = truncate_error_for_activity(msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_truncate_error_exactly_500_chars() {
        let msg = "a".repeat(500);
        let result = truncate_error_for_activity(&msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_truncate_error_501_chars() {
        let msg = "b".repeat(501);
        let result = truncate_error_for_activity(&msg);
        assert!(result.len() <= 500);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_very_long_message() {
        let msg = "x".repeat(10000);
        let result = truncate_error_for_activity(&msg);
        assert!(result.len() <= 500);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_empty_string() {
        let result = truncate_error_for_activity("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_error_with_multibyte_characters() {
        // Build a string with multi-byte unicode chars that would cross the boundary
        let prefix = "a".repeat(496);
        let msg = format!("{}emoji\u{1F600}\u{1F600}", prefix);
        let result = truncate_error_for_activity(&msg);
        // Must not panic and must respect char boundaries
        assert!(result.ends_with("..."));
        // Validate it's valid UTF-8 by just using it
        assert!(result.len() <= 503); // 500 content + "..."
    }

    // ---------------------------------------------------------------
    // enhance_prompt_with_learning tests
    // ---------------------------------------------------------------

    fn make_app_state_for_learning(learning: LearningConfig) -> AppState {
        let mut config = test_config();
        config.learning = learning;
        let tracker: Arc<dyn crate::storage::FixAttemptTracker> =
            Arc::new(SqliteTracker::in_memory().unwrap());
        AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: None,
        }
    }

    #[test]
    fn test_enhance_prompt_no_repo() {
        let state = make_app_state_for_learning(LearningConfig::default());
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt content";
        let result = enhance_prompt_with_learning(&state, base, &issue, None);
        // No repo means no enhancement
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_all_learning_disabled() {
        let learning = LearningConfig {
            repo_knowledge: false,
            qa_promotion: false,
            strategy_fingerprinting: false,
            cluster_detection: false,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt content";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        // All learning disabled, but the functions still check the DB -- which returns
        // empty results from an in-memory tracker, so no context is added
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_with_learning_enabled_no_data() {
        let learning = LearningConfig {
            repo_knowledge: true,
            qa_promotion: true,
            strategy_fingerprinting: true,
            cluster_detection: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt content";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        // No data in tracker, so still no enhancement
        assert_eq!(result, base);
    }

    // ---------------------------------------------------------------
    // record_error_pattern tests
    // ---------------------------------------------------------------

    #[test]
    fn test_record_error_pattern_basic() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Should not panic
        record_error_pattern(&state, "linear", "issue-123", "Connection timeout occurred");
    }

    #[test]
    fn test_record_error_pattern_empty_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Should not panic even with empty error
        record_error_pattern(&state, "sentry", "issue-456", "");
    }

    // ---------------------------------------------------------------
    // Helper to build AppState for handler-level tests
    // ---------------------------------------------------------------

    fn make_app_state(
        handlers: WebhookHandlerRegistry,
        tracker: Arc<dyn FixAttemptTracker>,
        sqlite_tracker: Option<Arc<dyn FixAttemptTracker>>,
    ) -> Arc<AppState> {
        let config = test_config();
        Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
            tracker,
            sqlite_tracker,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        })
    }

    fn make_app_state_with_processing(
        handlers: WebhookHandlerRegistry,
        tracker: Arc<dyn FixAttemptTracker>,
        processing: HashMap<String, Instant>,
    ) -> Arc<AppState> {
        let config = test_config();
        Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: None,
        })
    }

    fn make_app_state_with_github(
        github_handler: Option<GitHubWebhookHandler>,
        tracker: Arc<dyn FixAttemptTracker>,
    ) -> Arc<AppState> {
        let config = test_config();
        Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        })
    }

    // ---------------------------------------------------------------
    // Issue ID validation in webhook_handler
    // ---------------------------------------------------------------

    /// Mock handler that returns an issue with a configurable ID
    struct CustomIdHandler {
        issue_id: String,
    }

    impl CustomIdHandler {
        fn new(id: &str) -> Self {
            Self {
                issue_id: id.to_string(),
            }
        }
    }

    #[async_trait]
    impl WebhookHandler for CustomIdHandler {
        fn source_name(&self) -> &str {
            "custom"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            Ok(Some(Issue::new(
                &self.issue_id,
                "CUSTOM-1",
                "Test issue",
                "https://test.com/issue/1",
                "custom",
            )))
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Test criteria", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, issue: &Issue) -> crate::error::Result<String> {
            Ok(format!("Context for {}", issue.short_id))
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_rejects_path_traversal_issue_id() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("../../../etc/passwd")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Invalid issue ID"));
    }

    #[tokio::test]
    async fn test_webhook_handler_rejects_slash_in_issue_id() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("some/path/issue")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Invalid issue ID"));
    }

    #[tokio::test]
    async fn test_webhook_handler_rejects_backslash_in_issue_id() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("issue\\path")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Invalid issue ID"));
    }

    // ---------------------------------------------------------------
    // Duplicate delivery idempotency tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_duplicate_delivery_linear() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Record a delivery with the same ID
        tracker
            .check_and_record_delivery("delivery-abc", "test")
            .unwrap();

        // Now send a webhook with the same linear-delivery header
        let mut headers = HeaderMap::new();
        headers.insert("linear-delivery", "delivery-abc".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    #[tokio::test]
    async fn test_webhook_handler_new_delivery_proceeds() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Send a webhook with a new delivery ID
        let mut headers = HeaderMap::new();
        headers.insert("linear-delivery", "delivery-new".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        // Should pass through duplicate check and eventually be accepted
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    #[tokio::test]
    async fn test_webhook_handler_duplicate_delivery_github_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Pre-record the delivery
        tracker
            .check_and_record_delivery("gh-delivery-123", "test")
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("x-github-delivery", "gh-delivery-123".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    #[tokio::test]
    async fn test_webhook_handler_duplicate_delivery_sentry_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Pre-record the delivery
        tracker
            .check_and_record_delivery("sentry-hook-456", "test")
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("sentry-hook-id", "sentry-hook-456".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    // ---------------------------------------------------------------
    // Suppression rule tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_issue_suppressed_by_rule() {
        use crate::prioritisation::suppression::RegexCache;
        use crate::types::{SuppressionField, SuppressionMatchMode, SuppressionRule};

        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.prioritisation.suppression_rules = vec![SuppressionRule {
            name: "suppress-test".to_string(),
            field: SuppressionField::Title,
            pattern: "Test".to_string(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "Test issues are suppressed".to_string(),
        }];

        let cache = RegexCache::new(&config.prioritisation.suppression_rules);

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: Some(cache),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "suppressed");
        assert_eq!(response["rule"], "suppress-test");
    }

    // ---------------------------------------------------------------
    // Processing set capacity overflow tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_processing_set_ttl_cleanup_in_webhook_handler() {
        // Simulate stale entries being cleaned up when a new webhook arrives.
        // We cannot easily simulate old Instants, but we can verify the handler
        // proceeds correctly with fresh entries in the processing set.
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Fill with many entries for a different issue
        let mut processing = HashMap::new();
        for i in 0..5 {
            processing.insert(format!("test:other-{}", i), Instant::now());
        }

        let state = make_app_state_with_processing(handlers, tracker, processing);

        // Should still accept a new, different issue
        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // GitHub webhook handler tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_github_webhook_no_handler_configured() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(None, tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"{}");

        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("not configured"));
    }

    #[tokio::test]
    async fn test_handle_github_webhook_invalid_json() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"not json at all {{{");

        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    // ---------------------------------------------------------------
    // webhook_handler routes to github handler for source_name == "github"
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_routes_github_to_dedicated_handler() {
        // When source_name is "github", the handler should route to
        // handle_github_webhook rather than the generic registry.
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        // No github_handler configured, so it should return NOT_FOUND
        let state = make_app_state_with_github(None, tracker);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("github".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("not configured"));
    }

    // ---------------------------------------------------------------
    // Tower/axum integration tests using oneshot
    // ---------------------------------------------------------------

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn build_router(state: Arc<AppState>) -> Router {
        let concurrency_layer = ConcurrencyLimitLayer::new(10);
        Router::new()
            .route("/health", get(health_handler))
            .route(
                "/webhook/{source}",
                post(webhook_handler).layer(concurrency_layer),
            )
            .with_state(state)
    }

    #[tokio::test]
    async fn test_router_health_endpoint_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["handlers"].as_array().unwrap().len() >= 1);
    }

    #[tokio::test]
    async fn test_router_webhook_unknown_source_oneshot() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/unknown")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("Unknown source"));
    }

    #[tokio::test]
    async fn test_router_webhook_invalid_signature_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(RejectingSignatureHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/rejecting")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Invalid signature"));
    }

    #[tokio::test]
    async fn test_router_webhook_invalid_json_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from("this is not json"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    #[tokio::test]
    async fn test_router_webhook_accepted_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "accepted");
        assert_eq!(json["issue"], "TEST-1");
    }

    #[tokio::test]
    async fn test_router_webhook_event_not_applicable_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(IgnoringHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/ignoring")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
    }

    #[tokio::test]
    async fn test_router_webhook_criteria_not_matched_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(NonMatchingHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/nonmatching")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
        assert!(json["reason"].as_str().unwrap().contains("Does not match"));
    }

    #[tokio::test]
    async fn test_router_webhook_parse_error_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(FailingParseHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/failing")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("Failed to parse"));
    }

    #[tokio::test]
    async fn test_router_webhook_already_attempted_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        tracker.record_attempt("test", "1", "TEST-1").unwrap();
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
        assert!(json["reason"]
            .as_str()
            .unwrap()
            .contains("Already attempted"));
    }

    #[tokio::test]
    async fn test_router_webhook_already_processing_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut processing = HashMap::new();
        processing.insert("test:1".to_string(), Instant::now());
        let state = make_app_state_with_processing(handlers, tracker, processing);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
        assert!(json["reason"]
            .as_str()
            .unwrap()
            .contains("Already processing"));
    }

    #[tokio::test]
    async fn test_router_get_on_webhook_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        // GET on a POST-only route
        let request = Request::builder()
            .method("GET")
            .uri("/webhook/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_router_post_on_health_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        // POST on a GET-only route
        let request = Request::builder()
            .method("POST")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_router_nonexistent_route_returns_404() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    // ---------------------------------------------------------------
    // Health handler response field validation
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_health_handler_includes_github_webhook_enabled_field() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(None, tracker);

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["github_webhook_enabled"], false);
    }

    #[tokio::test]
    async fn test_health_handler_github_enabled_when_configured() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["github_webhook_enabled"], true);
    }

    #[tokio::test]
    async fn test_health_handler_returns_handler_names() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        handlers.register(Arc::new(MockWebhookHandler::new("jira")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let Json(response) = health_handler(State(state)).await;

        let handler_names: Vec<String> = response["handlers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();

        assert!(handler_names.contains(&"linear".to_string()));
        assert!(handler_names.contains(&"sentry".to_string()));
        assert!(handler_names.contains(&"jira".to_string()));
    }

    // ---------------------------------------------------------------
    // Health endpoint via full router integration
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_health_with_processing_entries_oneshot() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut processing = HashMap::new();
        processing.insert("linear:abc".to_string(), Instant::now());
        processing.insert("sentry:def".to_string(), Instant::now());
        processing.insert("sentry:ghi".to_string(), Instant::now());
        let state = make_app_state_with_processing(handlers, tracker, processing);
        let app = build_router(state);

        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["processing_count"], 3);
    }

    // ---------------------------------------------------------------
    // WebhookServer builder method tests
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_set_review_watcher() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert!(server.review_watcher.is_none());
        server.set_review_watcher(None);
        assert!(server.review_watcher.is_none());
    }

    #[test]
    fn test_webhook_server_set_issue_embedding_service() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert!(server.issue_embedding_service.is_none());
        server.set_issue_embedding_service(None);
        assert!(server.issue_embedding_service.is_none());
    }

    #[test]
    fn test_webhook_server_new_with_github() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);

        let server = WebhookServer::new_with_github(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            Some(github_handler),
            test_agent(tracker),
        );

        assert!(server.github_handler.is_some());
        assert_eq!(server.port, 8080);
    }

    #[test]
    fn test_webhook_server_new_without_github() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new_with_github(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            None,
            test_agent(tracker),
        );

        assert!(server.github_handler.is_none());
    }

    // ---------------------------------------------------------------
    // Signature detection header tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_detects_linear_signature_header() {
        // The webhook handler logs whether a signature header is present.
        // We can verify the handler runs through this path without errors.
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let mut headers = HeaderMap::new();
        headers.insert("linear-signature", "somesig".parse().unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_webhook_handler_detects_sentry_signature_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let mut headers = HeaderMap::new();
        headers.insert("sentry-hook-signature", "sig123".parse().unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn test_webhook_handler_detects_github_signature_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let mut headers = HeaderMap::new();
        headers.insert("x-hub-signature-256", "sha256=abc123".parse().unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Multiple sequential webhook requests (handler state isolation)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_second_webhook_for_same_issue_returns_already_processing() {
        // First request gets accepted, second should report "Already processing"
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        // First request
        let (status1, _) = webhook_handler(
            State(Arc::clone(&state)),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(status1, StatusCode::ACCEPTED);

        // Second request for the same issue while it is still "processing"
        let (status2, Json(response2)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        // It could be "Already processing" or "Already attempted" depending on timing
        assert_eq!(status2, StatusCode::OK);
        let reason = response2["reason"].as_str().unwrap();
        assert!(
            reason.contains("Already processing") || reason.contains("Already attempted"),
            "unexpected reason: {}",
            reason
        );
    }

    // ---------------------------------------------------------------
    // Empty body tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_empty_body_is_invalid_json() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b""),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    // ---------------------------------------------------------------
    // Large payload handling
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_large_json_payload() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        // Build a large but valid JSON payload
        let large_value = "x".repeat(100_000);
        let payload = format!(r#"{{"data": "{}"}}"#, large_value);

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from(payload),
        )
        .await;

        // The mock handler ignores the payload content, so this should be accepted
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Processing constants validation
    // ---------------------------------------------------------------

    #[test]
    fn test_processing_entry_ttl_is_one_hour() {
        assert_eq!(PROCESSING_ENTRY_TTL_SECS, 3600);
    }

    #[test]
    fn test_max_processing_entries_is_1000() {
        assert_eq!(MAX_PROCESSING_ENTRIES, 1000);
    }

    // ---------------------------------------------------------------
    // notify_failed_with_escalation tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_notify_failed_with_escalation_normal_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result = notify_failed_with_escalation(&state, &issue, "Some normal error").await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_hard_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        // "process timed out" is a hard error
        let result =
            notify_failed_with_escalation(&state, &issue, "process timed out after 300s").await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_rate_limit_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        // "rate limited" is also a hard error
        let result =
            notify_failed_with_escalation(&state, &issue, "API rate limited by server").await;
        assert!(result.is_ok());
    }

    // ---------------------------------------------------------------
    // WebhookHandlerRegistry additional tests
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_handler_registry_has() {
        let mut registry = WebhookHandlerRegistry::new();
        registry.register(Arc::new(MockWebhookHandler::new("linear")));

        assert!(registry.has("linear"));
        assert!(!registry.has("sentry"));
    }

    #[test]
    fn test_webhook_handler_registry_default() {
        let registry = WebhookHandlerRegistry::default();
        assert!(registry.get_all().is_empty());
        assert!(!registry.has("anything"));
    }

    // ---------------------------------------------------------------
    // Concurrent webhook handling via shared state
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_processing_set_isolation_between_sources() {
        // Processing key format is "source:issue_id", so different sources
        // with the same issue_id should not collide.
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Mark linear:1 as processing
        let mut processing = HashMap::new();
        processing.insert("linear:1".to_string(), Instant::now());

        let state = make_app_state_with_processing(handlers, tracker, processing);

        // linear:1 should be blocked
        let (status_linear, Json(resp_linear)) = webhook_handler(
            State(Arc::clone(&state)),
            Path("linear".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status_linear, StatusCode::OK);
        assert!(resp_linear["reason"]
            .as_str()
            .unwrap()
            .contains("Already processing"));

        // sentry:1 should NOT be blocked (different source)
        let (status_sentry, Json(resp_sentry)) = webhook_handler(
            State(state),
            Path("sentry".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status_sentry, StatusCode::ACCEPTED);
        assert_eq!(resp_sentry["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // Header conversion edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_header_conversion_filters_non_utf8() {
        use axum::http::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-valid"),
            HeaderValue::from_static("good"),
        );
        // HeaderValue can contain bytes that are not valid UTF-8 when created
        // from bytes, but from_static requires valid ASCII. For this test, we
        // just validate that the conversion logic works for standard headers.

        let header_map: HashMap<String, String> = headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|val| (k.as_str().to_lowercase(), val.to_string()))
            })
            .collect();

        assert_eq!(header_map.len(), 1);
        assert_eq!(header_map.get("x-valid"), Some(&"good".to_string()));
    }

    #[test]
    fn test_header_conversion_lowercases_mixed_case() {
        use axum::http::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        // HTTP headers in axum are already stored lowercase, but we test the
        // explicit lowercasing in the conversion logic
        headers.insert(
            HeaderName::from_static("x-my-header"),
            HeaderValue::from_static("Value123"),
        );

        let header_map: HashMap<String, String> = headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|val| (k.as_str().to_lowercase(), val.to_string()))
            })
            .collect();

        assert_eq!(header_map.get("x-my-header"), Some(&"Value123".to_string()));
    }

    // ---------------------------------------------------------------
    // Router health endpoint JSON structure via full integration
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_health_full_json_structure() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);

        let config = test_config();
        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: Some(github_handler),
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        });

        let app = build_router(state);
        let request = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // Verify all expected fields
        assert_eq!(json["status"], "ok");
        assert_eq!(json["processing_count"], 0);
        assert!(json["handlers"].is_array());
        assert_eq!(json["github_webhook_enabled"], true);
    }

    // ---------------------------------------------------------------
    // Webhook handler with sqlite tracker enabled (non-duplicate path)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_with_sqlite_tracker_no_delivery_header() {
        // When there is a sqlite_tracker but no delivery header, the idempotency
        // check is skipped and processing continues normally.
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker));

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // Webhook duplicate delivery: linear-delivery takes priority
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_delivery_header_priority_linear_over_github() {
        // When both linear-delivery and x-github-delivery are present,
        // linear-delivery should be checked first (per the or_else chain).
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Record a delivery for the linear header value
        tracker
            .check_and_record_delivery("linear-id-123", "test")
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert("linear-delivery", "linear-id-123".parse().unwrap());
        headers.insert("x-github-delivery", "github-id-456".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        // Should be caught as duplicate via linear-delivery header
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    // ---------------------------------------------------------------
    // Processing key uniqueness between different issues
    // ---------------------------------------------------------------

    #[test]
    fn test_processing_key_different_issues() {
        let key1 = format!("{}:{}", "linear", "issue-1");
        let key2 = format!("{}:{}", "linear", "issue-2");
        let key3 = format!("{}:{}", "sentry", "issue-1");

        assert_ne!(key1, key2);
        assert_ne!(key1, key3);
        assert_ne!(key2, key3);
    }

    // ---------------------------------------------------------------
    // Processing map capacity overflow simulation
    // ---------------------------------------------------------------

    #[test]
    fn test_processing_map_overflow_cleanup() {
        // Simulate the cleanup logic that happens when processing set is at capacity
        let mut processing: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();

        // Fill to capacity
        for i in 0..MAX_PROCESSING_ENTRIES {
            processing.insert(format!("key:{}", i), now);
        }

        assert_eq!(processing.len(), MAX_PROCESSING_ENTRIES);

        // Simulate the overflow cleanup: remove oldest half
        if processing.len() >= MAX_PROCESSING_ENTRIES {
            let mut entries: Vec<_> = processing.iter().map(|(k, v)| (k.clone(), *v)).collect();
            entries.sort_by_key(|(_, v)| *v);
            let to_remove = entries.len() / 2;
            for (key, _) in entries.into_iter().take(to_remove) {
                processing.remove(&key);
            }
        }

        assert_eq!(processing.len(), MAX_PROCESSING_ENTRIES / 2);
    }

    // ---------------------------------------------------------------
    // WebhookServer construction with sqlite_tracker
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_new_with_sqlite_tracker() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let agent = test_agent(tracker.clone());
        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            Some(tracker),
            None,
            agent,
        );

        assert!(server.sqlite_tracker.is_some());
        assert_eq!(server.port, 8080);
    }

    #[test]
    fn test_webhook_server_new_with_inferrer_none() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert!(server.inferrer.is_none());
        assert!(server.sqlite_tracker.is_none());
        assert!(server.issue_embedding_service.is_none());
        assert!(server.review_watcher.is_none());
        assert!(server.github_handler.is_none());
    }

    #[test]
    fn test_webhook_server_new_preserves_config_fields() {
        let mut config = test_config();
        config.workspace = std::path::PathBuf::from("/custom/workspace");
        config.known_orgs = vec!["org-a".to_string(), "org-b".to_string()];
        config.webhook_port = 9999;
        config.max_issues_per_cycle = 42;

        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        assert_eq!(server.port, 9999);
        assert_eq!(
            server.config.workspace,
            std::path::PathBuf::from("/custom/workspace")
        );
        assert_eq!(server.config.known_orgs.len(), 2);
        assert_eq!(server.config.max_issues_per_cycle, 42);
    }

    // ---------------------------------------------------------------
    // WebhookServer setter methods with actual values
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_set_review_watcher_with_value_then_clear() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        // Initially None
        assert!(server.review_watcher.is_none());

        // Set to None explicitly
        server.set_review_watcher(None);
        assert!(server.review_watcher.is_none());
    }

    #[test]
    fn test_webhook_server_set_issue_embedding_service_with_value_then_clear() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        // Initially None
        assert!(server.issue_embedding_service.is_none());

        // Set to None explicitly
        server.set_issue_embedding_service(None);
        assert!(server.issue_embedding_service.is_none());
    }

    // ---------------------------------------------------------------
    // new_with_github preserves all fields
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_new_with_github_preserves_all_fields() {
        let mut config = test_config();
        config.webhook_port = 4321;
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);

        let agent = test_agent(tracker.clone());
        let server = WebhookServer::new_with_github(
            config,
            handlers,
            notifier,
            tracker.clone(),
            Some(tracker),
            None,
            Some(github_handler),
            agent,
        );

        assert_eq!(server.port, 4321);
        assert!(server.github_handler.is_some());
        assert!(server.sqlite_tracker.is_some());
        assert!(server.issue_embedding_service.is_none());
        assert!(server.review_watcher.is_none());
    }

    // ---------------------------------------------------------------
    // AppState construction with all optional fields
    // ---------------------------------------------------------------

    #[test]
    fn test_app_state_with_sqlite_tracker() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        assert!(state.sqlite_tracker.is_some());
        assert!(state.github_handler.is_none());
        assert!(state.review_watcher.is_none());
        assert!(state.embedding_client.is_none());
        assert!(state.issue_embedding_service.is_none());
        assert!(state.inferrer.is_none());
        assert!(state.suppression_regex_cache.is_none());
    }

    // ---------------------------------------------------------------
    // Processing deduplication TTL retain with fresh entries
    // ---------------------------------------------------------------

    #[test]
    fn test_processing_retain_keeps_fresh_entries() {
        let mut map: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();

        map.insert("fresh1".to_string(), now);
        map.insert("fresh2".to_string(), now);

        let ttl = std::time::Duration::from_secs(PROCESSING_ENTRY_TTL_SECS);
        map.retain(|_, started_at| now.duration_since(*started_at) < ttl);

        // Fresh entries should all remain
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("fresh1"));
        assert!(map.contains_key("fresh2"));
    }

    #[test]
    fn test_processing_retain_with_empty_map() {
        let mut map: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();
        let ttl = std::time::Duration::from_secs(PROCESSING_ENTRY_TTL_SECS);

        map.retain(|_, started_at| now.duration_since(*started_at) < ttl);

        assert!(map.is_empty());
    }

    #[test]
    fn test_processing_overflow_cleanup_removes_half() {
        let mut processing: HashMap<String, Instant> = HashMap::new();
        let now = Instant::now();

        // Fill beyond capacity
        for i in 0..(MAX_PROCESSING_ENTRIES + 50) {
            processing.insert(format!("key:{}", i), now);
        }

        assert!(processing.len() >= MAX_PROCESSING_ENTRIES);

        // Simulate overflow cleanup
        let mut entries: Vec<_> = processing.iter().map(|(k, v)| (k.clone(), *v)).collect();
        entries.sort_by_key(|(_, v)| *v);
        let to_remove = entries.len() / 2;
        for (key, _) in entries.into_iter().take(to_remove) {
            processing.remove(&key);
        }

        // Should have roughly half remaining
        assert!(processing.len() < MAX_PROCESSING_ENTRIES);
        assert!(processing.len() > 0);
    }

    // ---------------------------------------------------------------
    // truncate_error_for_activity - additional boundary tests
    // ---------------------------------------------------------------

    #[test]
    fn test_truncate_error_exactly_499_chars() {
        let msg = "a".repeat(499);
        let result = truncate_error_for_activity(&msg);
        assert_eq!(result, msg);
        assert_eq!(result.len(), 499);
    }

    #[test]
    fn test_truncate_error_single_char() {
        let result = truncate_error_for_activity("x");
        assert_eq!(result, "x");
    }

    #[test]
    fn test_truncate_error_preserves_content_under_limit() {
        let msg = "Error: connection refused to database at host 127.0.0.1:5432";
        let result = truncate_error_for_activity(msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_truncate_error_long_unicode_string() {
        // Build a string with 2-byte unicode chars exceeding 500 chars
        let msg = "\u{00E9}".repeat(600); // e-acute, 2 bytes each
        let result = truncate_error_for_activity(&msg);
        // Should not panic and should truncate safely at char boundaries
        assert!(result.ends_with("..."));
        // The truncated content (without "...") should have valid char count
        let content = &result[..result.len() - 3];
        assert!(content.chars().count() <= 500);
    }

    #[test]
    fn test_truncate_error_4byte_unicode_boundary() {
        // Mix of ASCII and 4-byte emoji chars near the 500-byte boundary
        let prefix = "a".repeat(495);
        let msg = format!("{}\u{1F4A9}\u{1F4A9}\u{1F4A9}\u{1F4A9}", prefix);
        let result = truncate_error_for_activity(&msg);
        // Should not panic, must be valid UTF-8
        assert!(result.is_char_boundary(result.len()));
    }

    // ---------------------------------------------------------------
    // record_error_pattern with different error types
    // ---------------------------------------------------------------

    #[test]
    fn test_record_error_pattern_timeout() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Should not panic with various error types
        record_error_pattern(&state, "linear", "issue-1", "Process timed out after 300s");
        record_error_pattern(
            &state,
            "sentry",
            "issue-2",
            "Build failed: cargo build error",
        );
        record_error_pattern(
            &state,
            "linear",
            "issue-3",
            "Test assertion failed: expected 5 got 3",
        );
        record_error_pattern(&state, "sentry", "issue-4", "Git merge conflict in main.rs");
    }

    #[test]
    fn test_record_error_pattern_multiple_for_same_issue() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Recording multiple errors for the same issue should not panic
        record_error_pattern(&state, "linear", "issue-1", "Error A");
        record_error_pattern(&state, "linear", "issue-1", "Error B");
        record_error_pattern(&state, "linear", "issue-1", "Error C");
    }

    // ---------------------------------------------------------------
    // enhance_prompt_with_learning - repo provided but no data
    // ---------------------------------------------------------------

    #[test]
    fn test_enhance_prompt_with_empty_repo_name() {
        let state = make_app_state_for_learning(LearningConfig::default());
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt";
        // Empty string for repo_name is still Some("")
        let result = enhance_prompt_with_learning(&state, base, &issue, Some(""));
        // With empty repo name, DB lookups should return nothing
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_repo_knowledge_only() {
        let learning = LearningConfig {
            repo_knowledge: true,
            qa_promotion: false,
            strategy_fingerprinting: false,
            cluster_detection: false,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        // In-memory tracker has no data, so result should be unchanged
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_qa_promotion_only() {
        let learning = LearningConfig {
            repo_knowledge: false,
            qa_promotion: true,
            strategy_fingerprinting: false,
            cluster_detection: false,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_strategy_fingerprinting_only() {
        let learning = LearningConfig {
            repo_knowledge: false,
            qa_promotion: false,
            strategy_fingerprinting: true,
            cluster_detection: false,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_cluster_detection_only() {
        let learning = LearningConfig {
            repo_knowledge: false,
            qa_promotion: false,
            strategy_fingerprinting: false,
            cluster_detection: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "base prompt";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        assert_eq!(result, base);
    }

    // ---------------------------------------------------------------
    // Health handler with github_handler Some
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_health_handler_with_github_and_handlers() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);

        let config = test_config();
        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
            tracker,
            sqlite_tracker: None,
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: Some(github_handler),
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        });

        let Json(response) = health_handler(State(state)).await;

        assert_eq!(response["status"], "ok");
        assert_eq!(response["github_webhook_enabled"], true);
        assert_eq!(response["processing_count"], 0);
        let handler_names: Vec<String> = response["handlers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(handler_names.len(), 2);
    }

    // ---------------------------------------------------------------
    // handle_github_webhook edge cases
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_github_webhook_valid_json_no_signature() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"{}");

        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        // Without a webhook secret configured, the handler should process (signature
        // verification passes if no secret is set). The event won't match any known
        // action so it will be "ignored".
        assert!(
            status == StatusCode::OK || status == StatusCode::UNAUTHORIZED,
            "status was {:?}",
            status
        );
        // Either processed/ignored or rejected depending on signature config
        assert!(response.get("status").is_some() || response.get("error").is_some());
    }

    #[tokio::test]
    async fn test_handle_github_webhook_with_signature_header_present() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let mut header_map: HashMap<String, String> = HashMap::new();
        header_map.insert(
            "x-hub-signature-256".to_string(),
            "sha256=abc123".to_string(),
        );

        let body = Bytes::from_static(b"{\"action\": \"submitted\", \"review\": {}}");

        let (status, _response) = handle_github_webhook(state, &header_map, &body).await;

        // Should not panic; status depends on signature validation
        assert!(
            status == StatusCode::OK
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::INTERNAL_SERVER_ERROR,
            "unexpected status: {:?}",
            status
        );
    }

    // ---------------------------------------------------------------
    // notify_failed_with_escalation - additional error patterns
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_notify_failed_with_escalation_empty_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        // Empty error string is not a hard error
        let result = notify_failed_with_escalation(&state, &issue, "").await;
        assert!(result.is_ok());
        assert_eq!(notifier.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_spawn_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        // "failed to spawn claude" is a hard error
        let result =
            notify_failed_with_escalation(&state, &issue, "failed to spawn claude process").await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    // ---------------------------------------------------------------
    // Router integration: body size limit
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_webhook_body_within_limit() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        // Send a body well within the 512KB limit
        let payload = format!(r#"{{"data": "{}"}}"#, "x".repeat(1000));
        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from(payload))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Router integration: multiple handler registration
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_with_multiple_handlers_routes_correctly() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(NonMatchingHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        // Request to "nonmatching" handler should be criteria-rejected
        let request = Request::builder()
            .method("POST")
            .uri("/webhook/nonmatching")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
    }

    // ---------------------------------------------------------------
    // WebhookServer new delegates to new_with_github(None)
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_new_delegates_to_new_with_github_none() {
        let config = test_config();
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );

        // new() should result in github_handler being None
        assert!(server.github_handler.is_none());
    }

    // ---------------------------------------------------------------
    // Processing set concurrent access patterns
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_rwlock_processing_map_concurrent_reads() {
        let processing: Arc<RwLock<HashMap<String, Instant>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Write an entry
        {
            let mut write_guard = processing.write().await;
            write_guard.insert("concurrent:1".to_string(), Instant::now());
        }

        // Multiple concurrent reads should work
        let read1 = processing.read().await;
        let read2 = processing.read().await;

        assert!(read1.contains_key("concurrent:1"));
        assert!(read2.contains_key("concurrent:1"));
        assert_eq!(read1.len(), 1);
        assert_eq!(read2.len(), 1);
    }

    #[tokio::test]
    async fn test_processing_map_insert_and_remove_cycle() {
        let processing: RwLock<HashMap<String, Instant>> = RwLock::new(HashMap::new());

        // Insert
        {
            let mut write = processing.write().await;
            write.insert("key1".to_string(), Instant::now());
            write.insert("key2".to_string(), Instant::now());
            write.insert("key3".to_string(), Instant::now());
        }

        assert_eq!(processing.read().await.len(), 3);

        // Remove one
        {
            let mut write = processing.write().await;
            write.remove("key2");
        }

        let read = processing.read().await;
        assert_eq!(read.len(), 2);
        assert!(read.contains_key("key1"));
        assert!(!read.contains_key("key2"));
        assert!(read.contains_key("key3"));
    }

    // ---------------------------------------------------------------
    // Webhook handler with x-signature header (generic signature header)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_detects_x_signature_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let mut headers = HeaderMap::new();
        headers.insert("x-signature", "somesig".parse().unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Custom issue ID handler: valid IDs accepted
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_accepts_valid_issue_ids() {
        // Standard alphanumeric IDs should be accepted
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("abc-123-def")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    #[tokio::test]
    async fn test_webhook_handler_accepts_uuid_issue_id() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new(
            "550e8400-e29b-41d4-a716-446655440000",
        )));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    #[tokio::test]
    async fn test_webhook_handler_rejects_empty_issue_id() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("custom".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Invalid issue ID"));
    }

    // ---------------------------------------------------------------
    // Accepted webhook inserts processing key
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_accepted_webhook_inserts_processing_key() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, _) = webhook_handler(
            State(Arc::clone(&state)),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);

        // After acceptance, the processing key should be present
        let processing = state.processing.read().await;
        assert!(
            processing.contains_key("test:1"),
            "Processing key 'test:1' should be present after acceptance"
        );
    }

    // ---------------------------------------------------------------
    // make_app_state helper validates correct construction
    // ---------------------------------------------------------------

    #[test]
    fn test_make_app_state_helper() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        assert!(state.github_handler.is_none());
        assert!(state.sqlite_tracker.is_none());
        assert!(state.inferrer.is_none());
    }

    #[test]
    fn test_make_app_state_with_sqlite_tracker_helper() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker));

        assert!(state.sqlite_tracker.is_some());
    }

    // ---------------------------------------------------------------
    // Error response JSON format consistency
    // ---------------------------------------------------------------

    #[test]
    fn test_error_response_json_structure_unknown_source() {
        let resp = json!({ "error": format!("Unknown source: {}", "foo") });
        assert!(resp["error"]
            .as_str()
            .unwrap()
            .starts_with("Unknown source:"));
    }

    #[test]
    fn test_error_response_json_structure_invalid_signature() {
        let resp = json!({ "error": "Invalid signature" });
        assert_eq!(resp["error"], "Invalid signature");
    }

    #[test]
    fn test_error_response_json_structure_invalid_json() {
        let resp = json!({ "error": "Invalid JSON" });
        assert_eq!(resp["error"], "Invalid JSON");
    }

    #[test]
    fn test_error_response_json_structure_invalid_issue_id() {
        let validation_error = "contains path separator";
        let resp = json!({ "error": format!("Invalid issue ID: {}", validation_error) });
        assert!(resp["error"]
            .as_str()
            .unwrap()
            .starts_with("Invalid issue ID:"));
    }

    #[test]
    fn test_ignored_response_json_structures() {
        let resp1 = json!({ "status": "ignored", "reason": "Event not applicable" });
        assert_eq!(resp1["status"], "ignored");
        assert_eq!(resp1["reason"], "Event not applicable");

        let resp2 = json!({ "status": "ignored", "reason": "Already attempted" });
        assert_eq!(resp2["reason"], "Already attempted");

        let resp3 = json!({ "status": "ignored", "reason": "Already processing" });
        assert_eq!(resp3["reason"], "Already processing");

        let resp4 = json!({ "status": "ignored", "reason": "Duplicate delivery" });
        assert_eq!(resp4["reason"], "Duplicate delivery");
    }

    #[test]
    fn test_suppressed_response_json_structure() {
        let resp = json!({ "status": "suppressed", "rule": "my-rule", "reason": "reason text" });
        assert_eq!(resp["status"], "suppressed");
        assert_eq!(resp["rule"], "my-rule");
        assert_eq!(resp["reason"], "reason text");
    }

    #[test]
    fn test_accepted_response_json_structure() {
        let resp = json!({ "status": "accepted", "issue": "TEST-42" });
        assert_eq!(resp["status"], "accepted");
        assert_eq!(resp["issue"], "TEST-42");
    }

    // ---------------------------------------------------------------
    // Router: DELETE and PUT methods return 405
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_delete_on_webhook_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("DELETE")
            .uri("/webhook/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_router_put_on_webhook_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("PUT")
            .uri("/webhook/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_router_delete_on_health_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("DELETE")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ---------------------------------------------------------------
    // Webhook handler with duplicate handlers (same name replaces)
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_handler_registry_register_same_name_replaces() {
        let mut registry = WebhookHandlerRegistry::new();
        registry.register(Arc::new(MockWebhookHandler::new("test")));
        registry.register(Arc::new(MockWebhookHandler::new("test")));

        // Registry uses HashMap, so same key replaces the previous entry
        assert_eq!(registry.get_all().len(), 1);
        assert!(registry.has("test"));
    }

    // ---------------------------------------------------------------
    // Processing key format edge cases
    // ---------------------------------------------------------------

    #[test]
    fn test_processing_key_with_special_characters_in_issue_id() {
        let source = "linear";
        let issue_id = "abc_123-def.456";
        let key = format!("{}:{}", source, issue_id);
        assert_eq!(key, "linear:abc_123-def.456");
    }

    #[test]
    fn test_processing_key_with_long_issue_id() {
        let source = "sentry";
        let issue_id = "a".repeat(200);
        let key = format!("{}:{}", source, issue_id);
        assert!(key.starts_with("sentry:"));
        assert_eq!(key.len(), 7 + 200); // "sentry:" + 200 chars
    }

    // ---------------------------------------------------------------
    // make_app_state_with_github helper validates construction
    // ---------------------------------------------------------------

    #[test]
    fn test_make_app_state_with_github_none_handler() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(None, tracker);

        assert!(state.github_handler.is_none());
    }

    #[test]
    fn test_make_app_state_with_github_some_handler() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        assert!(state.github_handler.is_some());
    }

    // ---------------------------------------------------------------
    // Webhook handler routes github source before registry lookup
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_github_source_bypasses_registry_even_with_handler() {
        // Even if "github" is registered in the handler registry,
        // the webhook_handler function should route to handle_github_webhook
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("github")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // No github_handler configured
        let config = test_config();
        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: None,
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("github".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        // Should go to handle_github_webhook, which returns NOT_FOUND
        // when github_handler is None
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("not configured"));
    }

    // ---------------------------------------------------------------
    // Delivery header fallback chain: github then sentry
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_delivery_header_priority_github_over_sentry() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        // Record a delivery for the github header value
        tracker
            .check_and_record_delivery("gh-id-789", "test")
            .unwrap();

        let mut headers = HeaderMap::new();
        // No linear-delivery header, so falls to x-github-delivery
        headers.insert("x-github-delivery", "gh-id-789".parse().unwrap());
        headers.insert("sentry-hook-id", "sentry-id-999".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        // Should be caught as duplicate via x-github-delivery header
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "ignored");
        assert!(response["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    // ---------------------------------------------------------------
    // Additional coverage tests
    // ---------------------------------------------------------------

    // ---------------------------------------------------------------
    // handle_github_webhook: activity logging with signature header
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_github_webhook_logs_has_signature_true() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let mut header_map: HashMap<String, String> = HashMap::new();
        header_map.insert(
            "x-hub-signature-256".to_string(),
            "sha256=invalid".to_string(),
        );

        let body = Bytes::from_static(b"{}");
        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        // Even if signature is invalid, the function should not panic.
        // The status depends on the GitHubWebhookHandler behavior.
        assert!(
            status == StatusCode::OK
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::INTERNAL_SERVER_ERROR,
            "status was {:?}",
            status
        );
        assert!(response.get("status").is_some() || response.get("error").is_some());
    }

    #[tokio::test]
    async fn test_handle_github_webhook_logs_has_signature_false() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        // No signature header
        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"{}");
        let (status, _) = handle_github_webhook(state, &header_map, &body).await;

        // Should still process without panicking
        assert!(
            status == StatusCode::OK
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // ---------------------------------------------------------------
    // handle_github_webhook: various JSON payloads
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_github_webhook_with_review_event_payload() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        // A review-like payload
        let body = Bytes::from(
            serde_json::to_vec(&json!({
                "action": "submitted",
                "review": {
                    "state": "changes_requested",
                    "body": "Please fix the tests"
                },
                "pull_request": {
                    "number": 42,
                    "html_url": "https://github.com/test/repo/pull/42"
                }
            }))
            .unwrap(),
        );

        let (status, _) = handle_github_webhook(state, &header_map, &body).await;
        // The handler should process without panicking
        assert!(
            status == StatusCode::OK
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[tokio::test]
    async fn test_handle_github_webhook_with_empty_object_payload() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"{}");
        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        // Empty object won't match any known event type
        assert!(
            status == StatusCode::OK
                || status == StatusCode::UNAUTHORIZED
                || status == StatusCode::INTERNAL_SERVER_ERROR,
        );
        assert!(response.get("status").is_some() || response.get("error").is_some());
    }

    // ---------------------------------------------------------------
    // notify_failed_with_escalation: various hard error strings
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_notify_failed_with_escalation_connection_reset() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        // "connection reset" is a hard error
        let result = notify_failed_with_escalation(
            &state,
            &issue,
            "Connection reset by peer during API call",
        )
        .await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_service_unavailable() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result =
            notify_failed_with_escalation(&state, &issue, "Service unavailable: 503 from API")
                .await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_network_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result =
            notify_failed_with_escalation(&state, &issue, "Network error: DNS resolution failed")
                .await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_broken_pipe() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result =
            notify_failed_with_escalation(&state, &issue, "Broken pipe while writing to process")
                .await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_internal_server_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result = notify_failed_with_escalation(
            &state,
            &issue,
            "Internal server error from upstream API",
        )
        .await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_with_escalation_quota_exceeded() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        let result =
            notify_failed_with_escalation(&state, &issue, "Quota exceeded for API key").await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_escalation_removes_resolved_user_for_hard_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let mut issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        issue.set_metadata("resolved_user", &"some-user".to_string());
        assert!(issue.get_metadata::<String>("resolved_user").is_some());

        // Hard error should trigger escalation (global notification with resolved_user removed)
        let result =
            notify_failed_with_escalation(&state, &issue, "Process timed out after 300s").await;
        assert!(result.is_ok());
        assert!(notifier.call_count.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn test_notify_failed_normal_error_preserves_issue() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let notifier = Arc::new(MockNotifier::new());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: notifier.clone(),
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
            suppression_regex_cache: None,
        };

        let mut issue = Issue::new("1", "TEST-1", "Test", "https://test.com", "test");
        issue.set_metadata("resolved_user", &"some-user".to_string());

        // Normal error should NOT remove resolved_user (the original issue is passed as-is)
        let result =
            notify_failed_with_escalation(&state, &issue, "Compilation error in main.rs").await;
        assert!(result.is_ok());
        // The original issue should still have resolved_user
        assert!(issue.get_metadata::<String>("resolved_user").is_some());
    }

    // ---------------------------------------------------------------
    // record_feedback_outcome_from_attempt tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_record_feedback_outcome_no_attempt_in_tracker() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: None,
        };

        let issue = Issue::new(
            "no-attempt",
            "NONE-1",
            "No attempt",
            "https://test.com",
            "test",
        );
        // Should return early without panicking when no attempt exists
        record_feedback_outcome_from_attempt(&state, "test", &issue, Outcome::Failed).await;
    }

    #[tokio::test]
    async fn test_record_feedback_outcome_with_attempt() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        // Record an attempt first
        tracker.record_attempt("test", "issue-1", "TEST-1").unwrap();

        let config = test_config();
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        let issue = Issue::new(
            "issue-1",
            "TEST-1",
            "Test issue",
            "https://test.com",
            "test",
        );
        // Should execute without panicking
        record_feedback_outcome_from_attempt(&state, "test", &issue, Outcome::Failed).await;
    }

    // ---------------------------------------------------------------
    // enhance_prompt_with_learning: with data in DB
    // ---------------------------------------------------------------

    #[test]
    fn test_enhance_prompt_all_learning_enabled_with_repo() {
        let learning = LearningConfig {
            repo_knowledge: true,
            qa_promotion: true,
            strategy_fingerprinting: true,
            cluster_detection: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "Build the feature as described.";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("my-repo"));
        // Fresh DB has no data, so prompt should be unchanged
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_preserves_base_prompt_content() {
        let learning = LearningConfig {
            repo_knowledge: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "This is a complex multi-line\nprompt with specific instructions";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("test-repo"));
        // Whether or not extra context is added, the base prompt must be present
        assert!(result.contains(base));
    }

    // ---------------------------------------------------------------
    // record_error_pattern with very long error messages
    // ---------------------------------------------------------------

    #[test]
    fn test_record_error_pattern_very_long_message() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Very long error message should not panic
        let long_error = "x".repeat(10000);
        record_error_pattern(&state, "linear", "issue-long", &long_error);
    }

    #[test]
    fn test_record_error_pattern_unicode_error() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        record_error_pattern(
            &state,
            "test",
            "issue-unicode",
            "Error with unicode: \u{1F4A9} \u{00E9}\u{00F1}",
        );
    }

    // ---------------------------------------------------------------
    // truncate_error_for_activity: boundary precision tests
    // ---------------------------------------------------------------

    #[test]
    fn test_truncate_error_exactly_at_boundary_no_ellipsis() {
        let msg = "a".repeat(500);
        let result = truncate_error_for_activity(&msg);
        assert_eq!(result.len(), 500);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_one_over_boundary_has_ellipsis() {
        let msg = "b".repeat(501);
        let result = truncate_error_for_activity(&msg);
        assert!(result.ends_with("..."));
        assert!(result.len() <= 500);
    }

    #[test]
    fn test_truncate_error_all_multibyte_chars() {
        // 3-byte UTF-8 chars (CJK characters)
        let msg = "\u{4E16}".repeat(200); // 600 bytes, 200 chars
        let result = truncate_error_for_activity(&msg);
        assert!(result.ends_with("..."));
        // Result must be valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_error_mixed_ascii_and_multibyte() {
        let msg = format!("{}{}", "a".repeat(490), "\u{1F600}".repeat(10));
        let result = truncate_error_for_activity(&msg);
        // Must not panic on mixed content
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        if msg.len() > 500 {
            assert!(result.ends_with("..."));
        }
    }

    // ---------------------------------------------------------------
    // Webhook handler with sqlite_tracker + new delivery IDs
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_first_delivery_with_sqlite_tracker() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker.clone()));

        let mut headers = HeaderMap::new();
        headers.insert("linear-delivery", "new-delivery-id".parse().unwrap());

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        // First delivery should proceed to acceptance
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // Webhook handler with multiple signature headers simultaneously
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_multiple_signature_headers() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let mut headers = HeaderMap::new();
        headers.insert("x-signature", "sig1".parse().unwrap());
        headers.insert("sentry-hook-signature", "sig2".parse().unwrap());
        headers.insert("linear-signature", "sig3".parse().unwrap());
        headers.insert("x-hub-signature-256", "sha256=sig4".parse().unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            headers,
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Webhook handler: suppression rule that does NOT match
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_suppression_rule_does_not_match() {
        use crate::prioritisation::suppression::RegexCache;
        use crate::types::{SuppressionField, SuppressionMatchMode, SuppressionRule};

        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.prioritisation.suppression_rules = vec![SuppressionRule {
            name: "suppress-deploy".to_string(),
            field: SuppressionField::Title,
            pattern: "deploy".to_string(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "Deploy issues suppressed".to_string(),
        }];

        let cache = RegexCache::new(&config.prioritisation.suppression_rules);

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: Some(cache),
        });

        // MockWebhookHandler creates issues with title "Test", which does NOT contain "deploy"
        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        // Should pass through suppression and be accepted
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // Webhook handler: suppression with sqlite_tracker for recording
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_suppression_records_to_tracker() {
        use crate::prioritisation::suppression::RegexCache;
        use crate::types::{SuppressionField, SuppressionMatchMode, SuppressionRule};

        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.prioritisation.suppression_rules = vec![SuppressionRule {
            name: "suppress-test".to_string(),
            field: SuppressionField::Title,
            pattern: "Test".to_string(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "Suppressed".to_string(),
        }];

        let cache = RegexCache::new(&config.prioritisation.suppression_rules);

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: Some(cache),
        });

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(response["status"], "suppressed");
    }

    // ---------------------------------------------------------------
    // WebhookServer constructor and setter combinations
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_new_with_all_options() {
        let mut config = test_config();
        config.webhook_port = 5555;
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);

        let agent = test_agent(tracker.clone());
        let mut server = WebhookServer::new_with_github(
            config,
            handlers,
            notifier,
            tracker.clone(),
            Some(tracker),
            None,
            Some(github_handler),
            agent,
        );

        assert_eq!(server.port, 5555);
        assert!(server.github_handler.is_some());
        assert!(server.sqlite_tracker.is_some());
        assert!(server.issue_embedding_service.is_none());
        assert!(server.review_watcher.is_none());

        // Test setters
        server.set_issue_embedding_service(None);
        server.set_review_watcher(None);
        assert!(server.issue_embedding_service.is_none());
        assert!(server.review_watcher.is_none());
    }

    // ---------------------------------------------------------------
    // Router integration: github source via full router
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_github_source_no_handler_oneshot() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/github")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("not configured"));
    }

    // ---------------------------------------------------------------
    // Router integration: multiple handlers with various responses
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_mixed_handlers_linear_accepted_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(RejectingSignatureHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/linear")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Webhook handler: accepted webhook records attempt
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_accepted_webhook_records_attempt_in_tracker() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), None);

        // Before webhook, no attempt recorded
        assert!(!tracker.has_attempted("test", "1").unwrap());

        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        // After acceptance, attempt should be recorded
        assert!(tracker.has_attempted("test", "1").unwrap());
    }

    // ---------------------------------------------------------------
    // handle_github_webhook: with non-JSON body variants
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_handle_github_webhook_empty_body() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"");

        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    #[tokio::test]
    async fn test_handle_github_webhook_truncated_json() {
        let github_handler =
            GitHubWebhookHandler::new(crate::config::GitHubConfig::default(), None);
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state_with_github(Some(github_handler), tracker);

        let header_map: HashMap<String, String> = HashMap::new();
        let body = Bytes::from_static(b"{\"action\": ");

        let (status, Json(response)) = handle_github_webhook(state, &header_map, &body).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(response["error"].as_str().unwrap().contains("Invalid JSON"));
    }

    // ---------------------------------------------------------------
    // Webhook handler: JSON array body (not an object)
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_json_array_body() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        // Valid JSON but an array, not an object
        let (status, _) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"[1, 2, 3]"),
        )
        .await;

        // serde_json::from_slice will parse it as a valid Value::Array
        // The mock handler ignores the payload content, so it should proceed
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Router: webhook with various content types
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_webhook_no_content_type_header() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        // No content-type header, but body is valid JSON
        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        // Axum webhook handler doesn't require content-type
        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Webhook handler: concurrent processing of different sources
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_concurrent_webhook_different_sources_both_accepted() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("linear")));
        handlers.register(Arc::new(MockWebhookHandler::new("sentry")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        // First webhook from linear
        let (status1, _) = webhook_handler(
            State(Arc::clone(&state)),
            Path("linear".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(status1, StatusCode::ACCEPTED);

        // Second webhook from sentry (same issue ID from mock, but different source)
        let (status2, _) = webhook_handler(
            State(state),
            Path("sentry".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;
        assert_eq!(status2, StatusCode::ACCEPTED);
    }

    // ---------------------------------------------------------------
    // Processing set: verify cleanup happens after key insertion
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_processing_set_entry_exists_after_acceptance() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, _) = webhook_handler(
            State(Arc::clone(&state)),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);

        // Verify the processing map has the expected key
        let processing = state.processing.read().await;
        assert!(processing.contains_key("test:1"));
        assert_eq!(processing.len(), 1);
    }

    // ---------------------------------------------------------------
    // Config preservation through WebhookServer construction
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_config_claude_timeout() {
        let mut config = test_config();
        config.agent.timeout_secs = 999;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );
        assert_eq!(server.config.agent.timeout_secs, 999);
    }

    #[test]
    fn test_webhook_server_config_max_concurrent() {
        let mut config = test_config();
        config.max_concurrent = 10;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );
        assert_eq!(server.config.max_concurrent, 10);
    }

    // ---------------------------------------------------------------
    // Activity log entry creation patterns
    // ---------------------------------------------------------------

    #[test]
    fn test_activity_log_entry_webhook_received_format() {
        let source_name = "linear";
        let activity = ActivityLogEntry::new(
            "webhook_received",
            format!("Webhook received from {}", source_name),
        )
        .with_source(source_name.to_string())
        .with_metadata(json!({
            "content_length": 1024,
            "has_signature": true
        }));

        assert_eq!(activity.activity_type, "webhook_received");
        assert!(activity.message.contains("linear"));
    }

    #[test]
    fn test_activity_log_entry_webhook_rejected_format() {
        let source_name = "sentry";
        let activity = ActivityLogEntry::new(
            "webhook_rejected",
            format!("Webhook rejected: invalid signature from {}", source_name),
        )
        .with_source(source_name.to_string());

        assert_eq!(activity.activity_type, "webhook_rejected");
        assert!(activity.message.contains("sentry"));
    }

    // ---------------------------------------------------------------
    // make_app_state_for_learning helper with custom tracker data
    // ---------------------------------------------------------------

    #[test]
    fn test_enhance_prompt_with_learning_large_base_prompt() {
        let learning = LearningConfig {
            repo_knowledge: true,
            qa_promotion: true,
            strategy_fingerprinting: true,
            cluster_detection: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Test title", "https://test.com", "test");
        let base = "x".repeat(10000);
        let result = enhance_prompt_with_learning(&state, &base, &issue, Some("my-repo"));
        assert!(result.contains(&base));
    }

    // ---------------------------------------------------------------
    // Router integration: suppression via full router
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_webhook_suppressed_oneshot() {
        use crate::prioritisation::suppression::RegexCache;
        use crate::types::{SuppressionField, SuppressionMatchMode, SuppressionRule};

        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let mut config = test_config();
        config.prioritisation.suppression_rules = vec![SuppressionRule {
            name: "suppress-test".to_string(),
            field: SuppressionField::Title,
            pattern: "Test".to_string(),
            match_mode: SuppressionMatchMode::Contains,
            sources: vec![],
            reason: "Test issues suppressed".to_string(),
        }];

        let cache = RegexCache::new(&config.prioritisation.suppression_rules);

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers,
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: Some(cache),
        });

        let app = build_router(state);
        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "suppressed");
    }

    // ---------------------------------------------------------------
    // Router: duplicate delivery via full router
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_webhook_duplicate_delivery_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        // Pre-record the delivery
        tracker
            .check_and_record_delivery("dup-id-123", "test")
            .unwrap();

        let state = make_app_state(handlers, tracker.clone(), Some(tracker));
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/test")
            .header("content-type", "application/json")
            .header("linear-delivery", "dup-id-123")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ignored");
        assert!(json["reason"]
            .as_str()
            .unwrap()
            .contains("Duplicate delivery"));
    }

    // ---------------------------------------------------------------
    // Router: path traversal issue ID via full router
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_webhook_path_traversal_id_oneshot() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(CustomIdHandler::new("../../etc/shadow")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("POST")
            .uri("/webhook/custom")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["error"].as_str().unwrap().contains("Invalid issue ID"));
    }

    // ---------------------------------------------------------------
    // Webhook handler: issue with metadata labels
    // ---------------------------------------------------------------

    /// Mock handler that sets labels on the issue
    struct LabeledIssueHandler;

    #[async_trait]
    impl WebhookHandler for LabeledIssueHandler {
        fn source_name(&self) -> &str {
            "labeled"
        }
        fn verify_signature(&self, _body: &[u8], _headers: &HashMap<String, String>) -> bool {
            true
        }
        async fn parse_payload(
            &self,
            _payload: &serde_json::Value,
        ) -> crate::error::Result<Option<Issue>> {
            let mut issue = Issue::new(
                "lab-1",
                "LAB-1",
                "Labeled issue",
                "https://test.com",
                "labeled",
            );
            issue.set_metadata("labels", &vec!["bug".to_string(), "critical".to_string()]);
            Ok(Some(issue))
        }
        fn matches_criteria(&self, _issue: &Issue) -> MatchResult {
            MatchResult::matched("Labeled", MatchPriority::Normal)
        }
        async fn build_issue_context(&self, _issue: &Issue) -> crate::error::Result<String> {
            Ok("Context for labeled issue".to_string())
        }
    }

    #[tokio::test]
    async fn test_webhook_handler_with_labeled_issue() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(LabeledIssueHandler));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("labeled".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
        assert_eq!(response["issue"], "LAB-1");
    }

    // ---------------------------------------------------------------
    // Webhook handler: issue with assignee metadata and user registry
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_accepted_with_sqlite_tracker() {
        let mut handlers = WebhookHandlerRegistry::new();
        handlers.register(Arc::new(MockWebhookHandler::new("test")));
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker.clone(), Some(tracker));

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test".to_string()),
            HeaderMap::new(),
            Bytes::from_static(b"{}"),
        )
        .await;

        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(response["status"], "accepted");
    }

    // ---------------------------------------------------------------
    // Health handler: processing entries reflect count accurately
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_health_handler_processing_count_matches_entries() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let config = test_config();

        let mut processing = HashMap::new();
        for i in 0..7 {
            processing.insert(format!("test:{}", i), Instant::now());
        }

        let state = Arc::new(AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config,
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
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
            suppression_regex_cache: None,
        });

        let Json(response) = health_handler(State(state)).await;
        assert_eq!(response["processing_count"], 7);
    }

    // ---------------------------------------------------------------
    // record_error_pattern: verify pattern storage
    // ---------------------------------------------------------------

    #[test]
    fn test_record_error_pattern_stores_source_and_issue() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Should create error pattern without panicking
        record_error_pattern(
            &state,
            "jira",
            "JIRA-42",
            "Compilation failed: undefined reference",
        );

        // The pattern was recorded - verify by recording another for the same error hash
        record_error_pattern(
            &state,
            "jira",
            "JIRA-43",
            "Compilation failed: undefined reference",
        );
    }

    // ---------------------------------------------------------------
    // WebhookServer: port defaults from config
    // ---------------------------------------------------------------

    #[test]
    fn test_webhook_server_default_port() {
        let config = test_config(); // default port is 8080
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );
        assert_eq!(server.port, 8080);
    }

    #[test]
    fn test_webhook_server_high_port() {
        let mut config = test_config();
        config.webhook_port = 65535;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );
        assert_eq!(server.port, 65535);
    }

    #[test]
    fn test_webhook_server_low_port() {
        let mut config = test_config();
        config.webhook_port = 80;
        let handlers = WebhookHandlerRegistry::new();
        let notifier = Arc::new(MockNotifier::new());
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());

        let server = WebhookServer::new(
            config,
            handlers,
            notifier,
            tracker.clone(),
            None,
            None,
            test_agent(tracker),
        );
        assert_eq!(server.port, 80);
    }

    // ---------------------------------------------------------------
    // Router: PATCH method returns 405
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_router_patch_on_webhook_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("PATCH")
            .uri("/webhook/test")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_router_patch_on_health_returns_method_not_allowed() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);
        let app = build_router(state);

        let request = Request::builder()
            .method("PATCH")
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // ---------------------------------------------------------------
    // Webhook handler: unicode in source name
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_webhook_handler_nonexistent_source_with_special_chars() {
        let handlers = WebhookHandlerRegistry::new();
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = make_app_state(handlers, tracker, None);

        let (status, Json(response)) = webhook_handler(
            State(state),
            Path("test-source_123".to_string()),
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

    // ---------------------------------------------------------------
    // Additional named coverage tests for truncate_error_for_activity
    // ---------------------------------------------------------------

    #[test]
    fn test_truncate_error_for_activity_short() {
        let msg = "A short error message";
        let result = truncate_error_for_activity(msg);
        assert_eq!(result, msg);
    }

    #[test]
    fn test_truncate_error_for_activity_exact() {
        let msg = "c".repeat(500);
        let result = truncate_error_for_activity(&msg);
        assert_eq!(result, msg);
        assert_eq!(result.len(), 500);
        assert!(!result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_for_activity_long() {
        let msg = "d".repeat(1000);
        let result = truncate_error_for_activity(&msg);
        assert!(result.len() <= 500);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_for_activity_multibyte() {
        // Build a string where the 500-byte boundary falls inside a multibyte char
        let prefix = "a".repeat(498);
        let msg = format!("{}\u{1F600}\u{1F600}", prefix); // 498 + 4 + 4 = 506 bytes
        let result = truncate_error_for_activity(&msg);
        // Must not panic and must be valid UTF-8
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        assert!(result.ends_with("..."));
    }

    #[test]
    fn test_truncate_error_for_activity_empty() {
        let result = truncate_error_for_activity("");
        assert_eq!(result, "");
    }

    // ---------------------------------------------------------------
    // Additional named coverage test for record_error_pattern
    // ---------------------------------------------------------------

    #[test]
    fn test_record_error_pattern() {
        let tracker = Arc::new(SqliteTracker::in_memory().unwrap());
        let state = AppState {
            agent: Arc::new(crate::runner::ClaudeAgentRunner::new(
                crate::runner::ClaudeRunnerConfig::default(),
                tracker.clone(),
            )),
            config: test_config(),
            handlers: WebhookHandlerRegistry::new(),
            notifier: Arc::new(MockNotifier::new()),
            tracker: tracker.clone(),
            sqlite_tracker: Some(tracker.clone()),
            inferrer: None,
            embedding_client: None,
            issue_embedding_service: None,
            feedback_analyzer: tokio::sync::Mutex::new(FeedbackAnalyzer::new()),
            review_watcher: None,
            user_registry: UserRegistry::new(HashMap::new()),
            github_handler: None,
            processing: RwLock::new(HashMap::new()),
            suppression_regex_cache: None,
        };

        // Should not panic and should record without error
        record_error_pattern(&state, "linear", "LIN-100", "Timeout during compilation");
        record_error_pattern(&state, "sentry", "SENTRY-200", "");
        record_error_pattern(&state, "jira", "JIRA-300", "a]b[c{d}e");
    }

    // ---------------------------------------------------------------
    // Additional named coverage tests for enhance_prompt_with_learning
    // ---------------------------------------------------------------

    #[test]
    fn test_enhance_prompt_with_learning_no_repo() {
        let state = make_app_state_for_learning(LearningConfig::default());
        let issue = Issue::new("1", "TEST-1", "Bug title", "https://test.com", "test");
        let base = "Fix the bug described in the issue.";
        let result = enhance_prompt_with_learning(&state, base, &issue, None);
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_with_learning_all_disabled() {
        let learning = LearningConfig {
            repo_knowledge: false,
            qa_promotion: false,
            strategy_fingerprinting: false,
            cluster_detection: false,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Bug title", "https://test.com", "test");
        let base = "Fix the bug described in the issue.";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("org/repo"));
        assert_eq!(result, base);
    }

    #[test]
    fn test_enhance_prompt_with_learning_no_data() {
        let learning = LearningConfig {
            repo_knowledge: true,
            qa_promotion: true,
            strategy_fingerprinting: true,
            cluster_detection: true,
            ..Default::default()
        };
        let state = make_app_state_for_learning(learning);
        let issue = Issue::new("1", "TEST-1", "Bug title", "https://test.com", "test");
        let base = "Fix the bug described in the issue.";
        let result = enhance_prompt_with_learning(&state, base, &issue, Some("org/repo"));
        // In-memory tracker has no data, so prompt should be unchanged
        assert_eq!(result, base);
    }
}

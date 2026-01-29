//! HTTP server for webhooks.

use super::{WebhookHandler, WebhookHandlerRegistry};
use crate::config::Config;
use crate::error::Result;
use crate::inference::{resolve_repo_for_issue, RepoInferrer, RepoResolution};
use crate::notifier::Notifier;
use crate::repo::{GitOps, RepoIndex};
use crate::runner::{ClaudeRunner, ClaudeRunnerConfig};
use crate::storage::{FixAttemptTracker, SqliteTracker};
use crate::types::{validate_issue_id, ActivityLogEntry, Issue};
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
    claude: ClaudeRunner,
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
        let port = config.webhook_port;
        Self {
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker,
            inferrer,
            port,
        }
    }

    /// Build a repository inferrer from config.
    ///
    /// This uses the fallback mechanism: if `auto_discover_paths` is configured,
    /// it scans the local filesystem. Otherwise, if a GitHub token is configured,
    /// it fetches repos via the GitHub API.
    pub async fn build_inferrer(
        config: &Config,
        github_client: Option<&crate::github::GitHubClient>,
    ) -> Result<Option<RepoInferrer>> {
        if config.known_orgs.is_empty() {
            tracing::info!("No known_orgs configured, inference disabled");
            return Ok(None);
        }

        // Check if we have any discovery method available
        let has_local_paths = !config.auto_discover_paths.is_empty();
        let has_github_client = github_client.map(|c| c.is_enabled()).unwrap_or(false);

        if !has_local_paths && !has_github_client {
            tracing::info!(
                "No auto_discover_paths configured and no GitHub token available, inference disabled"
            );
            return Ok(None);
        }

        let index = RepoIndex::build_with_fallback(
            &config.known_orgs,
            &config.auto_discover_paths,
            github_client,
            &config.work_dir,
        )
        .await?;

        if index.is_empty() {
            tracing::warn!("Repository index is empty, no repos discovered");
            return Ok(None);
        }

        tracing::info!(
            repos = index.len(),
            files = index.total_files(),
            "Repository index built for inference"
        );

        Ok(Some(RepoInferrer::new(index)))
    }

    /// Start the server.
    pub async fn start(self) -> Result<()> {
        let state = Arc::new(AppState {
            claude: ClaudeRunner::new(
                ClaudeRunnerConfig {
                    timeout_secs: self.config.claude_timeout_secs,
                },
                self.tracker.clone(),
            ),
            config: self.config,
            handlers: self.handlers,
            notifier: self.notifier,
            tracker: self.tracker,
            sqlite_tracker: self.sqlite_tracker,
            inferrer: self.inferrer,
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

        let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", self.port)).await?;

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

    Json(json!({
        "status": "ok",
        "processing_count": processing_count,
        "handlers": handlers
    }))
}

async fn webhook_handler(
    State(state): State<Arc<AppState>>,
    Path(source_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<serde_json::Value>) {
    let handler = match state.handlers.get(&source_name) {
        Some(h) => h,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Unknown source: {}", source_name) })),
            );
        }
    };

    // Convert headers to HashMap
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|val| (k.as_str().to_lowercase(), val.to_string()))
        })
        .collect();

    // Log webhook received
    let has_signature = header_map.contains_key("x-signature")
        || header_map.contains_key("sentry-hook-signature")
        || header_map.contains_key("linear-signature");
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
    if state.tracker.has_attempted(&source_name, &issue.id) {
        return (
            StatusCode::OK,
            Json(json!({ "status": "ignored", "reason": "Already attempted" })),
        );
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
        if let Err(e) = process_issue(
            state_clone,
            handler_clone,
            issue,
            match_result,
            processing_key,
        )
        .await
        {
            tracing::error!(source = source_name.as_str(), error = %e, "Error processing webhook");
        }
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "accepted", "issue": short_id })),
    )
}

// Repository resolution is now handled by the inference engine (RepoInferrer).
// See src/inference/mod.rs for the new implementation.

async fn process_issue(
    state: Arc<AppState>,
    handler: Arc<dyn WebhookHandler>,
    issue: Issue,
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
            return Ok(());
        }
    };

    // Clone-on-demand: if the repo was discovered via API and doesn't exist locally, clone it
    if resolution.needs_clone() {
        if let (Some(github_url), Some(default_branch)) =
            (resolution.github_url(), resolution.default_branch())
        {
            tracing::info!(
                short_id = %issue.short_id,
                repo_path = %project_dir.display(),
                github_url = %github_url,
                "Repository not cloned locally, cloning now"
            );

            if let Err(e) =
                GitOps::ensure_repo_at_path(&project_dir, github_url, default_branch).await
            {
                tracing::error!(
                    short_id = %issue.short_id,
                    error = %e,
                    "Failed to clone repository, skipping issue"
                );
                // Clean up processing flag before returning
                let mut processing = state.processing.write().await;
                processing.remove(&processing_key);
                // Mark as failed
                state
                    .tracker
                    .mark_failed(source_name, &issue.id, &format!("Clone failed: {}", e))?;
                return Ok(());
            }

            tracing::info!(
                short_id = %issue.short_id,
                repo_path = %project_dir.display(),
                "Repository cloned successfully"
            );
        }
    }

    // Note: processing flag and attempt already recorded by handle_webhook before spawning

    let result = async {
        // Notify start
        state.notifier.notify_start(&issue).await?;

        // Build context and run Claude
        let context = handler.build_issue_context(&issue).await?;
        let claude_result = state.claude.run_fix(&issue, &context, &project_dir).await?;

        if claude_result.success {
            if let Some(pr_url) = claude_result.pr_url {
                tracing::info!(short_id = %issue.short_id, pr_url = %pr_url, "Success! PR created");
                state
                    .tracker
                    .mark_success(source_name, &issue.id, &pr_url)?;
                state.notifier.notify_success(&issue, &pr_url).await?;

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
                state.notifier.notify_completed(&issue).await?;

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
            state.notifier.notify_failed(&issue, error).await?;

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

    // Remove from processing
    {
        let mut processing = state.processing.write().await;
        processing.remove(&processing_key);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        DiscordConfig, EmailConfig, GitHubAppConfig, GitHubConfig, PushConfig, RegressionConfig,
        RetryConfig, SmsConfig,
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
            discord: DiscordConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            regression: RegressionConfig::default(),
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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
                },
                tracker.clone(),
            ),
            config,
            handlers,
            notifier,
            tracker,
            sqlite_tracker: None,
            inferrer: None,
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

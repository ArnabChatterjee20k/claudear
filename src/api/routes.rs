//! API route handlers for the dashboard.

use super::auth::*;
use crate::config::Config;
use crate::storage::FixAttemptTracker;
use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus, RegressionWatchStatus};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tower_http::services::{ServeDir, ServeFile};

/// Shared state for API handlers.
#[derive(Clone)]
pub struct ApiState {
    pub config: Config,
    pub tracker: Arc<dyn FixAttemptTracker>,
    /// Instant when the server started, for uptime calculation.
    pub start_time: Instant,
}

/// Create the API router.
pub fn create_api_router(config: Config, tracker: Arc<dyn FixAttemptTracker>) -> Router {
    create_api_router_with_dashboard(config, tracker, None)
}

/// Create the API router with optional dashboard static file serving.
pub fn create_api_router_with_dashboard(
    config: Config,
    tracker: Arc<dyn FixAttemptTracker>,
    dashboard_dir: Option<PathBuf>,
) -> Router {
    let state = ApiState {
        config,
        tracker,
        start_time: Instant::now(),
    };

    let api_routes = Router::new()
        .route("/api/health", get(health_handler))
        .route("/api/stats", get(stats_handler))
        .route("/api/stats/overview", get(overview_handler))
        .route("/api/attempts", get(attempts_handler))
        .route("/api/attempts/{id}", get(attempt_detail_handler))
        .route(
            "/api/attempts/{id}/detail",
            get(attempt_full_detail_handler),
        )
        .route("/api/sources", get(sources_handler))
        .route("/api/retries", get(retries_handler))
        .route("/api/activity", get(activity_handler))
        .route("/api/analytics/summary", get(analytics_summary_handler))
        .route("/api/metrics", get(metrics_handler))
        .route("/api/errors", get(errors_handler))
        .route("/api/prs", get(prs_handler))
        .route("/api/prs/analytics", get(pr_analytics_handler))
        .route("/api/feedback", get(feedback_handler))
        .route("/api/regressions", get(regressions_handler))
        .route(
            "/api/regressions/{id}/checks",
            get(regression_checks_handler),
        )
        .route("/api/experiments", get(experiments_handler))
        .route("/api/repos", get(repos_handler))
        .route("/api/repos/stats", get(repo_stats_handler))
        .route("/api/repos/dependencies", get(dependencies_handler))
        .route("/api/inference/stats", get(inference_stats_handler))
        .route("/api/inference/history", get(inference_history_handler))
        // Auth routes
        .route("/api/auth/login", axum::routing::post(login_handler))
        .route("/api/auth/logout", axum::routing::post(logout_handler))
        .route("/api/auth/me", axum::routing::get(me_handler))
        // User CRUD routes
        .route(
            "/api/users",
            axum::routing::get(list_users_handler).post(create_user_handler),
        )
        .route(
            "/api/users/{id}",
            axum::routing::get(get_user_handler)
                .put(update_user_handler)
                .delete(delete_user_handler),
        )
        .with_state(state);

    // If dashboard directory is provided, serve static files
    if let Some(dashboard_path) = dashboard_dir {
        let index_file = dashboard_path.join("index.html");
        let serve_dir =
            ServeDir::new(&dashboard_path).not_found_service(ServeFile::new(&index_file));

        api_routes.fallback_service(serve_dir)
    } else {
        api_routes
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    uptime_secs: u64,
    database: DatabaseStatus,
}

#[derive(Serialize)]
struct DatabaseStatus {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct OverviewResponse {
    stats: FixAttemptStats,
    success_rate: f64,
    merge_rate: f64,
    recent_attempts: Vec<AttemptSummary>,
    sources: Vec<SourceSummary>,
}

#[derive(Serialize, Clone)]
struct AttemptSummary {
    id: i64,
    source: String,
    short_id: String,
    title: String,
    status: String,
    pr_url: Option<String>,
    attempted_at: String,
    retry_count: u32,
}

#[derive(Serialize)]
struct SourceSummary {
    name: String,
    total: usize,
    success: usize,
    failed: usize,
    merged: usize,
    success_rate: f64,
}

#[derive(Serialize)]
struct AttemptsResponse {
    attempts: Vec<AttemptSummary>,
    total: usize,
    page: usize,
    per_page: usize,
}

#[derive(Serialize)]
struct SourcesResponse {
    sources: Vec<SourceInfo>,
}

#[derive(Serialize)]
struct SourceInfo {
    name: String,
    enabled: bool,
    config: serde_json::Value,
}

#[derive(Serialize)]
struct RetriesResponse {
    retryable: Vec<AttemptSummary>,
    ready: Vec<AttemptSummary>,
    max_retries: u32,
}

#[derive(Deserialize)]
struct AttemptsQuery {
    status: Option<String>,
    source: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
}

async fn health_handler(_user: AuthUser, State(state): State<ApiState>) -> Json<HealthResponse> {
    let uptime_secs = state.start_time.elapsed().as_secs();

    // Check database connectivity by attempting to get stats
    let database = match state.tracker.get_stats() {
        Ok(_) => DatabaseStatus {
            status: "ok".to_string(),
            error: None,
        },
        Err(e) => DatabaseStatus {
            status: "error".to_string(),
            error: Some(e.to_string()),
        },
    };

    let overall_status = if database.status == "ok" {
        "ok"
    } else {
        "degraded"
    };

    Json(HealthResponse {
        status: overall_status.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs,
        database,
    })
}

async fn stats_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<FixAttemptStats>, StatusCode> {
    state
        .tracker
        .get_stats()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn overview_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<OverviewResponse>, StatusCode> {
    let stats = state
        .tracker
        .get_stats()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Calculate rates
    let completed = stats.merged + stats.closed + stats.failed + stats.cannot_fix;
    let success_rate = if stats.total > 0 {
        (stats.success + stats.merged) as f64 / stats.total as f64 * 100.0
    } else {
        0.0
    };
    let merge_rate = if completed > 0 {
        stats.merged as f64 / completed as f64 * 100.0
    } else {
        0.0
    };

    // Get recent attempts (last 10)
    let recent = get_attempts(&state.tracker, Some(10));

    // Build source summaries
    let sources: Vec<SourceSummary> = stats
        .by_source
        .iter()
        .map(|(name, s)| {
            let rate = if s.total > 0 {
                (s.success + s.merged) as f64 / s.total as f64 * 100.0
            } else {
                0.0
            };
            SourceSummary {
                name: name.clone(),
                total: s.total,
                success: s.success,
                failed: s.failed,
                merged: s.merged,
                success_rate: rate,
            }
        })
        .collect();

    Ok(Json(OverviewResponse {
        stats,
        success_rate,
        merge_rate,
        recent_attempts: recent,
        sources,
    }))
}

async fn attempts_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<AttemptsQuery>,
) -> Result<Json<AttemptsResponse>, StatusCode> {
    let page = query.page.unwrap_or(1);
    let per_page = query.per_page.unwrap_or(20).min(100);

    // Get all attempts and filter
    let all_attempts = get_attempts(&state.tracker, None);

    let filtered: Vec<AttemptSummary> = all_attempts
        .into_iter()
        .filter(|a| {
            let status_match = query
                .status
                .as_ref()
                .map(|s| a.status.to_lowercase() == s.to_lowercase())
                .unwrap_or(true);
            let source_match = query
                .source
                .as_ref()
                .map(|s| a.source.to_lowercase() == s.to_lowercase())
                .unwrap_or(true);
            status_match && source_match
        })
        .collect();

    let total = filtered.len();
    let start = (page - 1) * per_page;
    let attempts: Vec<AttemptSummary> = filtered.into_iter().skip(start).take(per_page).collect();

    Ok(Json(AttemptsResponse {
        attempts,
        total,
        page,
        per_page,
    }))
}

async fn attempt_detail_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<FixAttempt>, StatusCode> {
    // We need to find the attempt by ID across all statuses
    for status in [
        FixAttemptStatus::Pending,
        FixAttemptStatus::Success,
        FixAttemptStatus::Failed,
        FixAttemptStatus::Merged,
        FixAttemptStatus::Closed,
        FixAttemptStatus::CannotFix,
    ] {
        if let Ok(attempts) = state.tracker.get_attempts_by_status(status) {
            if let Some(attempt) = attempts.into_iter().find(|a| a.id == id) {
                return Ok(Json(attempt));
            }
        }
    }

    Err(StatusCode::NOT_FOUND)
}

async fn sources_handler(_user: AuthUser, State(state): State<ApiState>) -> Json<SourcesResponse> {
    let mut sources = Vec::new();

    if let Some(ref linear) = state.config.linear {
        sources.push(SourceInfo {
            name: "linear".to_string(),
            enabled: linear.enabled,
            config: serde_json::json!({
                "trigger_labels": linear.trigger_labels,
                "trigger_states": linear.trigger_states,
                "has_webhook_secret": linear.webhook_secret.is_some(),
            }),
        });
    }

    if let Some(ref sentry) = state.config.sentry {
        sources.push(SourceInfo {
            name: "sentry".to_string(),
            enabled: sentry.enabled,
            config: serde_json::json!({
                "org_slug": sentry.org_slug,
                "project_slugs": sentry.project_slugs,
                "min_event_count": sentry.min_event_count,
                "has_client_secret": sentry.client_secret.is_some(),
            }),
        });
    }

    Json(SourcesResponse { sources })
}

async fn retries_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<RetriesResponse>, StatusCode> {
    use crate::retry::RetryManager;

    let max_retries = state.config.retry.max_retries;

    let retryable = state
        .tracker
        .get_retryable_issues(max_retries)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Create RetryManager to compute which attempts are ready for retry
    let retry_manager = RetryManager::new(state.config.retry.clone(), state.tracker.clone());

    let retryable_summaries: Vec<AttemptSummary> =
        retryable.iter().map(attempt_to_summary).collect();

    // Filter to find attempts that are ready for retry now
    let ready_summaries: Vec<AttemptSummary> = retryable
        .iter()
        .filter(|a| retry_manager.is_ready_for_retry(a))
        .map(attempt_to_summary)
        .collect();

    Ok(Json(RetriesResponse {
        retryable: retryable_summaries,
        ready: ready_summaries,
        max_retries,
    }))
}

fn attempt_to_summary(attempt: &FixAttempt) -> AttemptSummary {
    AttemptSummary {
        id: attempt.id,
        source: attempt.source.clone(),
        short_id: attempt.short_id.clone(),
        title: attempt.short_id.clone(), // We don't store title, use short_id
        status: attempt.status.to_string(),
        pr_url: attempt.pr_url.clone(),
        attempted_at: attempt.attempted_at.to_rfc3339(),
        retry_count: attempt.retry_count,
    }
}

/// Get attempts from tracker, optionally limited.
fn get_attempts(tracker: &Arc<dyn FixAttemptTracker>, limit: Option<usize>) -> Vec<AttemptSummary> {
    let mut all: Vec<FixAttempt> = Vec::new();

    for status in [
        FixAttemptStatus::Pending,
        FixAttemptStatus::Success,
        FixAttemptStatus::Failed,
        FixAttemptStatus::Merged,
        FixAttemptStatus::Closed,
        FixAttemptStatus::CannotFix,
    ] {
        if let Ok(attempts) = tracker.get_attempts_by_status(status) {
            all.extend(attempts);
        }
    }

    // Sort by attempted_at descending
    all.sort_by(|a, b| b.attempted_at.cmp(&a.attempted_at));

    let iter = all.into_iter().map(|a| attempt_to_summary(&a));

    match limit {
        Some(n) => iter.take(n).collect(),
        None => iter.collect(),
    }
}

// ─── New query types ──────────────────────────────────

#[derive(Deserialize)]
struct ActivityQuery {
    limit: Option<usize>,
    source: Option<String>,
}

#[derive(Deserialize)]
struct MetricsQuery {
    name: Option<String>,
    period: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ErrorsQuery {
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct PrsQuery {
    status: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct FeedbackQuery {
    source: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct RegressionsQuery {
    status: Option<String>,
}

#[derive(Deserialize)]
struct InferenceHistoryQuery {
    limit: Option<usize>,
}

// ─── New response types ──────────────────────────────────

#[derive(Serialize)]
struct AttemptDetailResponse {
    attempt: FixAttempt,
    executions: Vec<crate::types::ClaudeExecution>,
    reviews: Vec<crate::types::PrReviewRecord>,
    feedback: Option<crate::feedback::FixOutcome>,
}

// ─── New handlers ──────────────────────────────────

async fn attempt_full_detail_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<AttemptDetailResponse>, StatusCode> {
    let attempt = state
        .tracker
        .get_attempt_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let executions = state
        .tracker
        .get_executions_for_attempt(id)
        .unwrap_or_default();

    let reviews = state
        .tracker
        .get_reviews_for_attempt(id)
        .unwrap_or_default();

    let feedback = state
        .tracker
        .get_feedback_outcome_by_attempt(id)
        .unwrap_or(None);

    Ok(Json(AttemptDetailResponse {
        attempt,
        executions,
        reviews,
        feedback,
    }))
}

async fn activity_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<Vec<crate::types::ActivityLogEntry>>, StatusCode> {
    let limit = query.limit.unwrap_or(50).min(500);
    let source_filter = query.source.as_deref();

    state
        .tracker
        .get_recent_activities_filtered(limit, source_filter)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn analytics_summary_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<crate::types::AnalyticsSummary>, StatusCode> {
    state
        .tracker
        .get_analytics_summary()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn metrics_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<MetricsQuery>,
) -> Result<Json<Vec<crate::types::ProcessingMetric>>, StatusCode> {
    let metric_name = query.name.as_deref().unwrap_or("processing_time");
    let limit = query.limit.unwrap_or(100).min(1000);

    let since = query.period.as_deref().and_then(|p| {
        let duration = match p {
            "hour" => chrono::Duration::hours(1),
            "day" => chrono::Duration::days(1),
            "week" => chrono::Duration::days(7),
            "month" => chrono::Duration::days(30),
            _ => return None,
        };
        Some(chrono::Utc::now() - duration)
    });

    state
        .tracker
        .get_metrics(metric_name, since, limit)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn errors_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<ErrorsQuery>,
) -> Result<Json<Vec<crate::types::ErrorPattern>>, StatusCode> {
    let limit = query.limit.unwrap_or(50).min(200);

    state
        .tracker
        .get_error_patterns(limit)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn prs_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<PrsQuery>,
) -> Result<Json<Vec<crate::types::PrRecord>>, StatusCode> {
    let limit = query.limit.unwrap_or(100).min(500);

    state
        .tracker
        .list_prs(query.status.as_deref(), limit)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn pr_analytics_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<crate::types::PrAnalytics>, StatusCode> {
    state
        .tracker
        .get_pr_analytics()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn feedback_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<FeedbackQuery>,
) -> Result<Json<Vec<crate::feedback::FixOutcome>>, StatusCode> {
    let limit = query.limit.unwrap_or(50).min(200);

    state
        .tracker
        .get_feedback_outcomes(query.source.as_deref(), limit)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn regressions_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<RegressionsQuery>,
) -> Result<Json<Vec<crate::types::RegressionWatch>>, StatusCode> {
    match query.status.as_deref() {
        Some(status_str) => {
            let status: RegressionWatchStatus =
                status_str.parse().map_err(|_| StatusCode::BAD_REQUEST)?;
            state
                .tracker
                .get_regression_watches_by_status(status)
                .map(Json)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
        }
        None => state
            .tracker
            .get_all_regression_watches()
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR),
    }
}

async fn regression_checks_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<crate::types::RegressionCheck>>, StatusCode> {
    state
        .tracker
        .get_regression_checks(id)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn experiments_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::types::PromptExperiment>>, StatusCode> {
    state
        .tracker
        .get_active_experiments()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn repos_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::storage::StoredIndexedRepo>>, StatusCode> {
    state
        .tracker
        .list_indexed_repos()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn repo_stats_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<crate::storage::IndexStats>, StatusCode> {
    state
        .tracker
        .get_index_stats()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn dependencies_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<Vec<crate::storage::StoredDependency>>, StatusCode> {
    state
        .tracker
        .list_all_dependencies()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn inference_stats_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<crate::storage::InferenceStats>, StatusCode> {
    state
        .tracker
        .get_inference_stats()
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn inference_history_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<InferenceHistoryQuery>,
) -> Result<Json<Vec<crate::storage::InferenceHistoryEntry>>, StatusCode> {
    let limit = query.limit.unwrap_or(50).min(500);

    state
        .tracker
        .get_inference_history(limit)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        CascadeConfig, ClaudeConfig, DiscordConfig, EmailConfig, GitHubAppConfig, GitHubConfig,
        PushConfig, RegressionConfig, RetryConfig, SmsConfig,
    };
    use crate::storage::SqliteTracker;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use tower_cookies::CookieManagerLayer;

    fn test_config() -> Config {
        Config {
            work_dir: "/tmp/repos".into(),
            known_orgs: vec!["test-org".to_string()],
            auto_discover_paths: vec![],
            poll_interval_ms: 300_000,
            webhook_port: 3100,
            db_path: ":memory:".into(),
            max_issues_per_cycle: 5,
            max_concurrent: 1,
            processing_delay_ms: 5000,
            max_activity_entries: 100,
            ipc_timeout_secs: 30,
            claude_timeout_secs: 21600,
            claude: ClaudeConfig::default(),
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
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
        }
    }

    fn create_test_tracker() -> Arc<dyn FixAttemptTracker> {
        Arc::new(SqliteTracker::in_memory().unwrap())
    }

    /// Create an authenticated test router with CookieManagerLayer and a session cookie.
    /// Returns (router, session_cookie_value).
    fn create_authenticated_router(tracker: &Arc<dyn FixAttemptTracker>) -> (Router, String) {
        let config = test_config();

        // Create a test user and session
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("testpass", 4).unwrap(); // cost=4 for speed
        db.create_user("test@example.com", &password_hash, "Test User", "admin")
            .unwrap();
        let token = db.create_session(1, "2099-12-31 23:59:59").unwrap();

        let router = create_api_router(config, tracker.clone()).layer(CookieManagerLayer::new());

        (router, token)
    }

    /// Build an authenticated GET request with the session cookie.
    fn auth_get(uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("cookie", format!("claudear_session={}", token))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn test_health_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/health", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_stats_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/stats", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_overview_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/stats/overview", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_attempts_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_attempts_with_pagination() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts?page=1&per_page=10", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_attempts_with_filter() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get(
                "/api/attempts?status=success&source=linear",
                &token,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_attempt_detail_not_found() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts/99999", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_sources_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/sources", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_retries_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/retries", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn test_attempt_to_summary() {
        let attempt = FixAttempt {
            id: 1,
            source: "linear".to_string(),
            issue_id: "123".to_string(),
            short_id: "PROJ-123".to_string(),
            status: FixAttemptStatus::Success,
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            github_repo: None,
            github_pr_number: None,
            error_message: None,
            attempted_at: chrono::Utc::now(),
            resolved_at: None,
            merged_at: None,
            retry_count: 0,
            last_retry_at: None,
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let summary = attempt_to_summary(&attempt);
        assert_eq!(summary.id, 1);
        assert_eq!(summary.source, "linear");
        assert_eq!(summary.short_id, "PROJ-123");
        assert_eq!(summary.status, "success");
        assert!(summary.pr_url.is_some());
    }

    #[test]
    fn test_health_response_serialization() {
        let response = HealthResponse {
            status: "ok".to_string(),
            version: "1.0.0".to_string(),
            uptime_secs: 3600,
            database: DatabaseStatus {
                status: "ok".to_string(),
                error: None,
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("ok"));
        assert!(json.contains("1.0.0"));
        assert!(json.contains("3600"));
        assert!(json.contains("database"));
    }

    #[test]
    fn test_health_response_with_database_error() {
        let response = HealthResponse {
            status: "degraded".to_string(),
            version: "1.0.0".to_string(),
            uptime_secs: 100,
            database: DatabaseStatus {
                status: "error".to_string(),
                error: Some("Connection failed".to_string()),
            },
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("degraded"));
        assert!(json.contains("error"));
        assert!(json.contains("Connection failed"));
    }

    #[test]
    fn test_attempts_response_serialization() {
        let response = AttemptsResponse {
            attempts: vec![],
            total: 0,
            page: 1,
            per_page: 20,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"total\":0"));
        assert!(json.contains("\"page\":1"));
    }

    #[test]
    fn test_source_summary_serialization() {
        let summary = SourceSummary {
            name: "linear".to_string(),
            total: 100,
            success: 80,
            failed: 10,
            merged: 70,
            success_rate: 80.0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("linear"));
        assert!(json.contains("100"));
        assert!(json.contains("80.0"));
    }

    #[test]
    fn test_source_info_serialization() {
        let info = SourceInfo {
            name: "sentry".to_string(),
            enabled: true,
            config: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains("sentry"));
        assert!(json.contains("true"));
        assert!(json.contains("value"));
    }

    #[test]
    fn test_overview_response_serialization() {
        let overview = OverviewResponse {
            stats: FixAttemptStats::default(),
            success_rate: 85.5,
            merge_rate: 75.0,
            recent_attempts: vec![],
            sources: vec![],
        };
        let json = serde_json::to_string(&overview).unwrap();
        assert!(json.contains("85.5"));
        assert!(json.contains("stats"));
    }

    #[test]
    fn test_retries_response_serialization() {
        let retries = RetriesResponse {
            retryable: vec![],
            ready: vec![],
            max_retries: 3,
        };
        let json = serde_json::to_string(&retries).unwrap();
        assert!(json.contains("retryable"));
        assert!(json.contains("3"));
    }

    #[test]
    fn test_sources_response_serialization() {
        let sources = SourcesResponse {
            sources: vec![SourceInfo {
                name: "linear".to_string(),
                enabled: true,
                config: serde_json::json!({}),
            }],
        };
        let json = serde_json::to_string(&sources).unwrap();
        assert!(json.contains("linear"));
    }

    #[test]
    fn test_attempt_summary_serialization() {
        let summary = AttemptSummary {
            id: 1,
            source: "linear".to_string(),
            short_id: "PROJ-1".to_string(),
            title: "Fix bug".to_string(),
            status: "success".to_string(),
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            attempted_at: "2024-01-01T00:00:00Z".to_string(),
            retry_count: 0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("linear"));
        assert!(json.contains("PROJ-1"));
        assert!(json.contains("success"));
    }

    #[test]
    fn test_attempts_query_deserialization() {
        let query: AttemptsQuery = serde_json::from_str(
            r#"{
            "page": 2,
            "per_page": 50,
            "status": "success",
            "source": "linear"
        }"#,
        )
        .unwrap();
        assert_eq!(query.page, Some(2));
        assert_eq!(query.per_page, Some(50));
        assert_eq!(query.status, Some("success".to_string()));
        assert_eq!(query.source, Some("linear".to_string()));
    }

    #[test]
    fn test_attempts_query_defaults() {
        let query: AttemptsQuery = serde_json::from_str("{}").unwrap();
        assert!(query.page.is_none());
        assert!(query.per_page.is_none());
        assert!(query.status.is_none());
        assert!(query.source.is_none());
    }

    #[tokio::test]
    async fn test_404_for_unknown_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/unknown", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_health_response_content() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/health", &token))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"status\":\"ok\""));
        assert!(body_str.contains("version"));
        assert!(body_str.contains("uptime_secs"));
        assert!(body_str.contains("database"));
    }

    #[tokio::test]
    async fn test_stats_response_content() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/stats", &token))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let stats: FixAttemptStats = serde_json::from_slice(&body).unwrap();
        assert_eq!(stats.total, 0);
    }

    #[tokio::test]
    async fn test_attempts_response_content() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts", &token))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"page\":1"));
        assert!(body_str.contains("\"per_page\":20"));
    }

    #[tokio::test]
    async fn test_attempts_pagination_limits() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // Test that per_page is capped at 100
        let response = router
            .oneshot(auth_get("/api/attempts?per_page=200", &token))
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"per_page\":100")); // Should be capped
    }

    #[test]
    fn test_attempt_to_summary_without_pr_url() {
        let attempt = FixAttempt {
            id: 2,
            source: "sentry".to_string(),
            issue_id: "456".to_string(),
            short_id: "SENTRY-456".to_string(),
            status: FixAttemptStatus::Failed,
            pr_url: None,
            github_repo: None,
            github_pr_number: None,
            error_message: Some("Error message".to_string()),
            attempted_at: chrono::Utc::now(),
            resolved_at: None,
            merged_at: None,
            retry_count: 2,
            last_retry_at: Some(chrono::Utc::now()),
            issue_labels: vec![],
            parent_attempt_id: None,
            cascade_repo: None,
        };

        let summary = attempt_to_summary(&attempt);
        assert_eq!(summary.id, 2);
        assert_eq!(summary.source, "sentry");
        assert!(summary.pr_url.is_none());
        assert_eq!(summary.retry_count, 2);
        assert_eq!(summary.status, "failed");
    }

    #[test]
    fn test_source_summary_zero_values() {
        let summary = SourceSummary {
            name: "empty".to_string(),
            total: 0,
            success: 0,
            failed: 0,
            merged: 0,
            success_rate: 0.0,
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("empty"));
        assert!(json.contains("0"));
    }
}

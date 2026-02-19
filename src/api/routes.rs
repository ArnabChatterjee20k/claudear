//! API route handlers for the dashboard.

use super::auth::*;
use crate::config::Config;
use crate::retry::RetryManager;
use crate::storage::{FixAttemptTracker, SqliteTracker};
use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus, RegressionWatchStatus};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path, Query, State, WebSocketUpgrade,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
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
    /// Path to the config file on disk (for read/write from dashboard).
    pub config_path: PathBuf,
    /// Watch channel for real-time indexing progress pushed from SQLite hooks.
    pub indexing_rx: tokio::sync::watch::Receiver<crate::storage::IndexingProgress>,
    /// General-purpose storage directory for user uploads (avatars, etc.).
    pub storage_dir: PathBuf,
}

/// Create the API router.
pub fn create_api_router(
    config: Config,
    tracker: Arc<dyn FixAttemptTracker>,
    config_path: PathBuf,
    indexing_rx: tokio::sync::watch::Receiver<crate::storage::IndexingProgress>,
) -> Router {
    create_api_router_with_dashboard(config, tracker, config_path, indexing_rx, None)
}

/// Create the API router with optional dashboard static file serving.
pub fn create_api_router_with_dashboard(
    config: Config,
    tracker: Arc<dyn FixAttemptTracker>,
    config_path: PathBuf,
    indexing_rx: tokio::sync::watch::Receiver<crate::storage::IndexingProgress>,
    dashboard_dir: Option<PathBuf>,
) -> Router {
    let storage_dir = config.storage_dir.clone();

    // Ensure avatar upload directory exists
    let avatars_dir = storage_dir.join("avatars");
    if let Err(e) = std::fs::create_dir_all(&avatars_dir) {
        tracing::warn!(error = %e, path = %avatars_dir.display(), "Failed to create avatars directory");
    }

    let state = ApiState {
        config,
        tracker,
        start_time: Instant::now(),
        config_path,
        indexing_rx,
        storage_dir: storage_dir.clone(),
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
        .route(
            "/api/attempts/{id}/logs/{execution_id}/{stream}",
            get(attempt_execution_log_handler),
        )
        .route("/api/sources", get(sources_handler))
        .route("/api/retries", get(retries_handler))
        .route("/api/activity", get(activity_handler))
        .route("/api/analytics/summary", get(analytics_summary_handler))
        .route("/api/metrics", get(metrics_handler))
        .route("/api/errors", get(errors_handler))
        .route("/api/issues", get(issues_handler))
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
        .route(
            "/api/repos/indexing-progress",
            get(indexing_progress_handler),
        )
        .route("/api/repos/dependencies", get(dependencies_handler))
        .route("/api/inference/stats", get(inference_stats_handler))
        .route("/api/inference/history", get(inference_history_handler))
        .route("/api/telemetry/overview", get(telemetry_overview_handler))
        .route(
            "/api/telemetry/timeseries",
            get(telemetry_timeseries_handler),
        )
        .route("/api/telemetry/pipeline", get(telemetry_pipeline_handler))
        .route("/api/telemetry/latency", get(telemetry_latency_handler))
        // Auth routes
        .route("/api/auth/login", axum::routing::post(login_handler))
        .route("/api/auth/logout", axum::routing::post(logout_handler))
        .route("/api/auth/me", axum::routing::get(me_handler))
        .route(
            "/api/auth/profile",
            axum::routing::put(update_profile_handler),
        )
        .route(
            "/api/auth/avatar",
            axum::routing::post(upload_avatar_handler)
                .layer(axum::extract::DefaultBodyLimit::max(6 * 1024 * 1024)),
        )
        // Config routes (admin only)
        .route(
            "/api/config",
            axum::routing::get(get_config_handler).put(put_config_handler),
        )
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
        .with_state(state)
        .nest_service("/avatars", ServeDir::new(avatars_dir));

    // If dashboard directory is provided, serve from filesystem (development override)
    if let Some(dashboard_path) = dashboard_dir {
        let index_file = dashboard_path.join("index.html");
        let serve_dir =
            ServeDir::new(&dashboard_path).not_found_service(ServeFile::new(&index_file));

        api_routes.fallback_service(serve_dir)
    } else if super::embedded::has_dashboard() {
        // Serve the embedded dashboard compiled into the binary
        api_routes.fallback_service(tower::service_fn(super::embedded::embedded_fallback))
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
    time_savings: Option<crate::types::TimeSavings>,
    agent_spawns_today: i64,
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
        Err(e) => {
            tracing::error!("Database health check failed: {}", e);
            DatabaseStatus {
                status: "error".to_string(),
                error: Some("Database connection failed".to_string()),
            }
        }
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

    // Get recent attempts (last 10) from SQL directly when available.
    let recent = if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
        db.list_recent_attempts(10)
            .map(|records| {
                records
                    .into_iter()
                    .map(|attempt| attempt_to_summary(&attempt))
                    .collect()
            })
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        get_attempts(&state.tracker, Some(10))
    };

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

    let now_utc = chrono::Utc::now();
    let seven_days_ago = (now_utc - chrono::Duration::days(7))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let twenty_four_h_ago = (now_utc - chrono::Duration::hours(24))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();

    let time_savings = state
        .tracker
        .get_time_savings(&seven_days_ago, state.config.dashboard.hours_per_fix, "7d")
        .ok();

    let agent_spawns_today = state
        .tracker
        .get_agent_spawn_count(&twenty_four_h_ago)
        .unwrap_or(0);

    Ok(Json(OverviewResponse {
        stats,
        success_rate,
        merge_rate,
        recent_attempts: recent,
        sources,
        time_savings,
        agent_spawns_today,
    }))
}

async fn attempts_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<AttemptsQuery>,
) -> Result<Json<AttemptsResponse>, StatusCode> {
    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(20).min(100);
    let status_filter = query.status.as_ref().map(|s| s.to_lowercase());
    let source_filter = query.source.as_ref().map(|s| s.to_lowercase());
    let status_filter = status_filter.as_deref();
    let source_filter = source_filter.as_deref();

    let (attempts, total) = if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>()
    {
        let offset = (page - 1) * per_page;
        let rows = db
            .list_attempts(status_filter, source_filter, per_page, offset)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let total = db
            .count_attempts(status_filter, source_filter)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let attempts = rows
            .into_iter()
            .map(|attempt| attempt_to_summary(&attempt))
            .collect();
        (attempts, total)
    } else {
        // Fallback for non-SQLite tracker implementations.
        let all_attempts = get_attempts(&state.tracker, None);
        let filtered: Vec<AttemptSummary> = all_attempts
            .into_iter()
            .filter(|a| {
                let status_match = status_filter.map(|s| a.status == s).unwrap_or(true);
                let source_match = source_filter.map(|s| a.source == s).unwrap_or(true);
                status_match && source_match
            })
            .collect();
        let total = filtered.len();
        let start = (page - 1) * per_page;
        let attempts = filtered.into_iter().skip(start).take(per_page).collect();
        (attempts, total)
    };

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
    state
        .tracker
        .get_attempt_by_id(id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
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
    if let (Some(db), Some(max)) = (tracker.as_any().downcast_ref::<SqliteTracker>(), limit) {
        if let Ok(attempts) = db.list_recent_attempts(max) {
            return attempts
                .into_iter()
                .map(|a| attempt_to_summary(&a))
                .collect();
        }
    }

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

/// Get raw attempt records from tracker for telemetry aggregation.
fn get_attempt_records(tracker: &Arc<dyn FixAttemptTracker>) -> Vec<FixAttempt> {
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

    all
}

/// Get raw attempt records since a timestamp for telemetry aggregation.
fn get_attempt_records_since(
    tracker: &Arc<dyn FixAttemptTracker>,
    since: DateTime<Utc>,
) -> Vec<FixAttempt> {
    if let Some(db) = tracker.as_any().downcast_ref::<SqliteTracker>() {
        if let Ok(attempts) = db.list_attempts_since(since) {
            return attempts;
        }
    }

    get_attempt_records(tracker)
        .into_iter()
        .filter(|a| a.attempted_at >= since)
        .collect()
}

fn compute_processing_time_summary(
    metrics: Vec<crate::types::ProcessingMetric>,
) -> ProcessingTimeSummary {
    let values: Vec<f64> = metrics.into_iter().map(|m| m.metric_value).collect();
    compute_processing_value_summary(values)
}

fn compute_processing_value_summary(mut values: Vec<f64>) -> ProcessingTimeSummary {
    if values.is_empty() {
        return ProcessingTimeSummary::default();
    }

    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let samples = values.len() as i64;
    let sum: f64 = values.iter().sum();
    let avg_secs = Some(sum / samples as f64);
    let max_secs = values.last().copied();

    let percentile = |p: f64| -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        let rank = ((values.len() as f64 - 1.0) * p).round() as usize;
        values.get(rank).copied()
    };

    ProcessingTimeSummary {
        samples,
        avg_secs,
        p50_secs: percentile(0.50),
        p95_secs: percentile(0.95),
        p99_secs: percentile(0.99),
        max_secs,
    }
}

fn parse_telemetry_period(period: Option<&str>, default: &str) -> (String, Duration) {
    match period.unwrap_or(default).to_lowercase().as_str() {
        "hour" => ("hour".to_string(), Duration::hours(1)),
        "day" => ("day".to_string(), Duration::days(1)),
        "month" => ("month".to_string(), Duration::days(30)),
        "week" => ("week".to_string(), Duration::days(7)),
        _ => match default {
            "hour" => ("hour".to_string(), Duration::hours(1)),
            "day" => ("day".to_string(), Duration::days(1)),
            "month" => ("month".to_string(), Duration::days(30)),
            _ => ("week".to_string(), Duration::days(7)),
        },
    }
}

fn sum_metric_values(metrics: &[crate::types::ProcessingMetric]) -> f64 {
    metrics.iter().map(|m| m.metric_value).sum()
}

fn average_metric_value(metrics: &[crate::types::ProcessingMetric]) -> Option<f64> {
    if metrics.is_empty() {
        return None;
    }
    Some(sum_metric_values(metrics) / metrics.len() as f64)
}

fn max_metric_value(metrics: &[crate::types::ProcessingMetric]) -> Option<f64> {
    metrics
        .iter()
        .map(|m| m.metric_value)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn latest_metric_value(metrics: &[crate::types::ProcessingMetric]) -> Option<f64> {
    metrics.first().map(|m| m.metric_value)
}

fn ratio(numerator: f64, denominator: f64) -> Option<f64> {
    if denominator > 0.0 {
        Some(numerator / denominator)
    } else {
        None
    }
}

fn summarize_window(
    attempts: &[FixAttempt],
    window: &str,
    start: DateTime<Utc>,
) -> TelemetryWindowMetric {
    let in_window: Vec<&FixAttempt> = attempts
        .iter()
        .filter(|a| a.attempted_at >= start)
        .collect();

    let processed = in_window
        .iter()
        .filter(|a| a.status != FixAttemptStatus::Pending)
        .count() as i64;
    let successful = in_window
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                FixAttemptStatus::Success | FixAttemptStatus::Merged
            )
        })
        .count() as i64;
    let failed = in_window
        .iter()
        .filter(|a| {
            matches!(
                a.status,
                FixAttemptStatus::Failed | FixAttemptStatus::Closed | FixAttemptStatus::CannotFix
            )
        })
        .count() as i64;
    let merged = in_window
        .iter()
        .filter(|a| a.status == FixAttemptStatus::Merged)
        .count() as i64;

    let success_rate = if processed > 0 {
        (successful as f64 / processed as f64) * 100.0
    } else {
        0.0
    };
    let error_rate = if processed > 0 {
        (failed as f64 / processed as f64) * 100.0
    } else {
        0.0
    };

    let hours = match window {
        "1h" => 1.0,
        "24h" => 24.0,
        "7d" => 24.0 * 7.0,
        _ => 1.0,
    };
    let throughput_per_hour = if hours > 0.0 {
        processed as f64 / hours
    } else {
        0.0
    };

    TelemetryWindowMetric {
        window: window.to_string(),
        processed,
        successful,
        failed,
        merged,
        success_rate,
        error_rate,
        throughput_per_hour,
    }
}

fn floor_to_bucket(timestamp: DateTime<Utc>, bucket_minutes: i64) -> DateTime<Utc> {
    let bucket_seconds = (bucket_minutes.max(1) * 60).max(60);
    let ts = timestamp.timestamp();
    let floored = ts - ts.rem_euclid(bucket_seconds);
    Utc.timestamp_opt(floored, 0).single().unwrap_or(timestamp)
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
struct IssuesQuery {
    source: Option<String>,
    page: Option<usize>,
    per_page: Option<usize>,
}

#[derive(Serialize)]
struct IssueSummary {
    id: i64,
    source: String,
    issue_id: String,
    short_id: Option<String>,
    title: Option<String>,
    description: Option<String>,
    url: Option<String>,
    priority: Option<String>,
    status: Option<String>,
    labels: Option<Vec<String>>,
    has_embedding: bool,
    created_at: String,
    updated_at: Option<String>,
}

#[derive(Serialize)]
struct IssuesResponse {
    issues: Vec<IssueSummary>,
    total: usize,
    page: usize,
    per_page: usize,
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

#[derive(Deserialize)]
struct TelemetryTimeseriesQuery {
    period: Option<String>,
    bucket_minutes: Option<i64>,
}

#[derive(Deserialize)]
struct TelemetryPeriodQuery {
    period: Option<String>,
}

// ─── New response types ──────────────────────────────────

#[derive(Serialize)]
struct AttemptDetailResponse {
    attempt: FixAttempt,
    executions: Vec<crate::types::ClaudeExecution>,
    reviews: Vec<crate::types::PrReviewRecord>,
    feedback: Option<crate::feedback::FixOutcome>,
}

#[derive(Serialize)]
struct AttemptExecutionLogResponse {
    attempt_id: i64,
    execution_id: i64,
    stream: String,
    path: Option<String>,
    content: Option<String>,
    truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryWindowMetric {
    window: String,
    processed: i64,
    successful: i64,
    failed: i64,
    merged: i64,
    success_rate: f64,
    error_rate: f64,
    throughput_per_hour: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryQueueMetrics {
    pending_attempts: i64,
    retryable_attempts: i64,
    ready_retries: i64,
    open_prs: i64,
    watches_awaiting_release: i64,
    watches_monitoring: i64,
    watches_resolved: i64,
    watches_regressed: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
struct ProcessingTimeSummary {
    samples: i64,
    avg_secs: Option<f64>,
    p50_secs: Option<f64>,
    p95_secs: Option<f64>,
    p99_secs: Option<f64>,
    max_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryProcessingTime {
    all_time: ProcessingTimeSummary,
    last_24h: ProcessingTimeSummary,
}

#[derive(Debug, Clone, Serialize, Default)]
struct SourceTelemetry {
    source: String,
    total: i64,
    pending: i64,
    success: i64,
    failed: i64,
    merged: i64,
    closed: i64,
    cannot_fix: i64,
    retryable: i64,
    success_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryOverviewResponse {
    generated_at: String,
    uptime_secs: u64,
    windows: Vec<TelemetryWindowMetric>,
    queue: TelemetryQueueMetrics,
    processing_time: TelemetryProcessingTime,
    source_breakdown: Vec<SourceTelemetry>,
    top_errors: Vec<crate::types::ErrorPattern>,
    activity_last_hour: HashMap<String, i64>,
    metric_counts_last_24h: HashMap<String, i64>,
    diagnostics: Option<crate::storage::DiagnosticCounts>,
    pr_analytics: crate::types::PrAnalytics,
    agent_spawns_today: i64,
    agent_spawns_this_week: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryTimeseriesPoint {
    bucket_start: String,
    total: i64,
    pending: i64,
    success: i64,
    failed: i64,
    merged: i64,
    closed: i64,
    cannot_fix: i64,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryTimeseriesResponse {
    period: String,
    bucket_minutes: i64,
    generated_at: String,
    points: Vec<TelemetryTimeseriesPoint>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryPipelineTotals {
    fetched: f64,
    matched: f64,
    queued: f64,
    processed: f64,
    pr_created: f64,
    retries_found: f64,
    retries_executed: f64,
    retries_failed: f64,
    pr_status_checks: f64,
    pr_status_merged: f64,
    pr_status_closed: f64,
    pr_status_errors: f64,
    regression_watches_created: f64,
    auto_resolved_on_merge: f64,
    cascade_triggered: f64,
    cascade_failed: f64,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryPipelineConversion {
    match_rate: Option<f64>,
    queue_rate: Option<f64>,
    processing_rate: Option<f64>,
    pr_yield_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryPollLoad {
    poll_cycles: i64,
    avg_cycle_secs: Option<f64>,
    p95_cycle_secs: Option<f64>,
    active_avg: Option<f64>,
    active_max: Option<f64>,
    pending_avg: Option<f64>,
    pending_max: Option<f64>,
    total_latest: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct TelemetryPipelineSource {
    source: String,
    fetched: f64,
    matched: f64,
    queued: f64,
    processed: f64,
    pr_created: f64,
    retries_executed: f64,
    retries_failed: f64,
    match_rate: Option<f64>,
    queue_rate: Option<f64>,
    processing_rate: Option<f64>,
    pr_yield_rate: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryPipelineResponse {
    generated_at: String,
    period: String,
    totals: TelemetryPipelineTotals,
    conversion: TelemetryPipelineConversion,
    poll_load: TelemetryPollLoad,
    per_source: Vec<TelemetryPipelineSource>,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryLatencyByStatus {
    status: String,
    summary: ProcessingTimeSummary,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryLatencyHistogramBucket {
    label: String,
    upper_bound_secs: Option<f64>,
    count: i64,
}

#[derive(Debug, Clone, Serialize)]
struct TelemetryLatencyResponse {
    generated_at: String,
    period: String,
    overall: ProcessingTimeSummary,
    by_status: Vec<TelemetryLatencyByStatus>,
    histogram: Vec<TelemetryLatencyHistogramBucket>,
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

async fn attempt_execution_log_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Path((attempt_id, execution_id, stream)): Path<(i64, i64, String)>,
) -> Result<Json<AttemptExecutionLogResponse>, StatusCode> {
    if stream != "stdout" && stream != "stderr" && stream != "events" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let attempt_exists = state
        .tracker
        .get_attempt_by_id(attempt_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_some();
    if !attempt_exists {
        return Err(StatusCode::NOT_FOUND);
    }

    let execution = if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
        db.get_execution_for_attempt(attempt_id, execution_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?
    } else {
        state
            .tracker
            .get_executions_for_attempt(attempt_id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .into_iter()
            .find(|e| e.id == execution_id)
            .ok_or(StatusCode::NOT_FOUND)?
    };

    let log_path = match stream.as_str() {
        "stdout" => execution.stdout_log_path.clone(),
        "stderr" => execution.stderr_log_path.clone(),
        "events" => execution.event_log_path.clone(),
        _ => None,
    };

    let fallback_preview = match stream.as_str() {
        "stdout" => execution.stdout_preview.clone(),
        "stderr" => execution.stderr_preview.clone(),
        "events" => None,
        _ => None,
    };

    // Validate that the log path is within the expected log root directory
    // to prevent path traversal attacks via crafted database records.
    if let Some(ref path) = log_path {
        let log_root = crate::runner::resolve_log_root();
        let canonical_root = tokio::fs::canonicalize(&log_root)
            .await
            .unwrap_or(std::path::absolute(&log_root).unwrap_or_else(|_| log_root.clone()));
        match tokio::fs::canonicalize(path).await {
            Ok(canonical_path) => {
                if !canonical_path.starts_with(&canonical_root) {
                    tracing::warn!(
                        path = %path,
                        log_root = %canonical_root.display(),
                        "Execution log path traversal blocked"
                    );
                    return Err(StatusCode::FORBIDDEN);
                }
            }
            Err(_) => {
                // File doesn't exist or can't be resolved; fall through to normal read
                // which will produce a fallback_preview
            }
        }
    }

    let mut truncated = false;
    let content = if let Some(path) = &log_path {
        match tokio::fs::read_to_string(path).await {
            Ok(raw) => {
                let (value, is_truncated) = tail_utf8(&raw, 200_000);
                truncated = is_truncated;
                Some(value)
            }
            Err(e) => {
                tracing::warn!(
                    attempt_id = attempt_id,
                    execution_id = execution_id,
                    stream = %stream,
                    path = %path,
                    error = %e,
                    "Failed to read execution log file"
                );
                fallback_preview
            }
        }
    } else {
        fallback_preview
    };

    Ok(Json(AttemptExecutionLogResponse {
        attempt_id,
        execution_id,
        stream,
        path: log_path,
        content,
        truncated,
    }))
}

fn tail_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }

    let start = value.len().saturating_sub(max_bytes);
    let safe_start = value
        .char_indices()
        .find(|(idx, _)| *idx >= start)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    (format!("...[truncated]\n{}", &value[safe_start..]), true)
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
    let mut summary = state
        .tracker
        .get_analytics_summary()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    summary.avg_time_to_pr_mins = state.tracker.get_avg_time_to_pr().unwrap_or(None);

    let thirty_days_ago = (chrono::Utc::now() - chrono::Duration::days(30))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    summary.cost_estimate = state
        .tracker
        .get_cost_estimate(
            &thirty_days_ago,
            state.config.dashboard.cost_per_minute,
            "30d",
        )
        .ok();
    summary.mttr_trend = state.tracker.get_mttr_trend(8).unwrap_or_default();
    summary.repo_leaderboard = state.tracker.get_repo_leaderboard().unwrap_or_default();

    Ok(Json(summary))
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

async fn issues_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<IssuesQuery>,
) -> Result<Json<IssuesResponse>, StatusCode> {
    let db = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;

    let page = query.page.unwrap_or(1).max(1);
    let per_page = query.per_page.unwrap_or(100).min(500);
    let offset = (page - 1) * per_page;

    let total = db
        .count_issues(query.source.as_deref())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let rows = db
        .list_issues(query.source.as_deref(), per_page, offset)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let issues: Vec<IssueSummary> = rows
        .into_iter()
        .map(|ie| {
            let labels: Option<Vec<String>> = ie
                .labels
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            IssueSummary {
                id: ie.id,
                source: ie.source,
                issue_id: ie.issue_id,
                short_id: ie.short_id,
                title: ie.title,
                description: ie.description,
                url: ie.url,
                priority: ie.priority,
                status: ie.status,
                labels,
                has_embedding: ie.embedding.is_some(),
                created_at: ie.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                updated_at: ie
                    .updated_at
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string()),
            }
        })
        .collect();

    Ok(Json(IssuesResponse {
        issues,
        total,
        page,
        per_page,
    }))
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
    let mut analytics = state
        .tracker
        .get_pr_analytics()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    analytics.avg_time_to_pr_mins = state.tracker.get_avg_time_to_pr().unwrap_or(None);
    analytics.rejection_reasons = state.tracker.get_rejection_reasons(10).unwrap_or_default();

    Ok(Json(analytics))
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

async fn indexing_progress_handler(
    _user: AuthUser,
    ws: WebSocketUpgrade,
    State(state): State<ApiState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| indexing_progress_ws(socket, state.indexing_rx))
}

async fn indexing_progress_ws(
    mut socket: WebSocket,
    mut rx: tokio::sync::watch::Receiver<crate::storage::IndexingProgress>,
) {
    // Send current state immediately on connect
    {
        let progress = rx.borrow_and_update().clone();
        if let Ok(json) = serde_json::to_string(&progress) {
            if socket.send(Message::Text(json.into())).await.is_err() {
                return;
            }
        }
    }

    let mut ping_interval = tokio::time::interval(std::time::Duration::from_secs(30));
    // The first tick fires immediately; consume it so we don't send a spurious ping.
    ping_interval.tick().await;

    // Wait for changes pushed from SQLite write hooks
    loop {
        tokio::select! {
            // Watch channel notified — a write method updated progress
            result = rx.changed() => {
                if result.is_err() {
                    // Sender dropped (server shutting down)
                    break;
                }
                let progress = rx.borrow_and_update().clone();
                let json = match serde_json::to_string(&progress) {
                    Ok(j) => j,
                    Err(_) => break,
                };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
                // If idle, we've sent the final state — close gracefully
                if progress.status == "idle" {
                    let _ = socket.send(Message::Close(None)).await;
                    break;
                }
            }
            // Keepalive ping every 30 seconds
            _ = ping_interval.tick() => {
                if socket.send(Message::Ping(vec![].into())).await.is_err() {
                    break;
                }
            }
            // Client sent something (close frame or disconnect)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {} // ignore pings, text, etc.
                }
            }
        }
    }
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

async fn telemetry_overview_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<TelemetryOverviewResponse>, StatusCode> {
    let now = Utc::now();
    let stats = state
        .tracker
        .get_stats()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let recent_attempts = get_attempt_records_since(&state.tracker, now - Duration::days(7));

    let windows = vec![
        summarize_window(&recent_attempts, "1h", now - Duration::hours(1)),
        summarize_window(&recent_attempts, "24h", now - Duration::hours(24)),
        summarize_window(&recent_attempts, "7d", now - Duration::days(7)),
    ];

    let retryable = state
        .tracker
        .get_retryable_issues(state.config.retry.max_retries)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let retry_manager = RetryManager::new(state.config.retry.clone(), state.tracker.clone());
    let ready_retries = retryable
        .iter()
        .filter(|a| retry_manager.is_ready_for_retry(a))
        .count() as i64;

    let pr_analytics = state
        .tracker
        .get_pr_analytics()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let watches = state
        .tracker
        .get_all_regression_watches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut queue = TelemetryQueueMetrics {
        pending_attempts: stats.pending as i64,
        retryable_attempts: retryable.len() as i64,
        ready_retries,
        open_prs: pr_analytics.open,
        ..TelemetryQueueMetrics::default()
    };
    for watch in &watches {
        match watch.status {
            RegressionWatchStatus::AwaitingRelease => queue.watches_awaiting_release += 1,
            RegressionWatchStatus::Monitoring => queue.watches_monitoring += 1,
            RegressionWatchStatus::Resolved => queue.watches_resolved += 1,
            RegressionWatchStatus::Regressed => queue.watches_regressed += 1,
        }
    }

    let processing_time_all = state
        .tracker
        .get_metrics("processing_time", None, 20_000)
        .unwrap_or_default();
    let processing_time_last_24h = state
        .tracker
        .get_metrics("processing_time", Some(now - Duration::hours(24)), 20_000)
        .unwrap_or_default();
    let processing_time = TelemetryProcessingTime {
        all_time: compute_processing_time_summary(processing_time_all),
        last_24h: compute_processing_time_summary(processing_time_last_24h),
    };

    let top_errors = state.tracker.get_error_patterns(10).unwrap_or_default();

    let activity_last_hour =
        if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
            db.get_activity_type_counts_since(now - Duration::hours(1))
                .unwrap_or_default()
        } else {
            let mut counts = HashMap::new();
            for entry in state
                .tracker
                .get_recent_activities_filtered(5_000, None)
                .unwrap_or_default()
                .into_iter()
                .filter(|a| a.timestamp >= now - Duration::hours(1))
            {
                *counts.entry(entry.activity_type).or_insert(0) += 1;
            }
            counts
        };

    let metric_names = [
        "processing_time",
        "batch_processed",
        "pr_created",
        "issues_fetched",
        "issues_matched",
        "issues_queued",
        "poll_cycle_duration_secs",
        "poll_sources",
        "active_processing",
        "pending_attempts",
        "total_attempts",
        "ready_retries_found",
        "ready_retries_executed_total",
        "ready_retries_failed_total",
        "ready_retry_executed",
        "ready_retry_failed",
        "pr_status_checks",
        "pr_status_merged",
        "pr_status_closed",
        "pr_status_errors",
        "regression_watches_created",
        "auto_resolved_on_merge",
        "cascade_triggered",
        "cascade_failed",
    ];
    let mut metric_counts_last_24h: HashMap<String, i64> = HashMap::new();
    if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
        let counts = db
            .get_metric_counts_since(&metric_names, now - Duration::hours(24))
            .unwrap_or_default();
        for metric_name in metric_names {
            metric_counts_last_24h.insert(
                metric_name.to_string(),
                counts.get(metric_name).copied().unwrap_or(0),
            );
        }
    } else {
        for metric_name in metric_names {
            let count = state
                .tracker
                .get_metrics(metric_name, Some(now - Duration::hours(24)), 20_000)
                .map(|m| m.len() as i64)
                .unwrap_or(0);
            metric_counts_last_24h.insert(metric_name.to_string(), count);
        }
    }

    let mut by_source: HashMap<String, SourceTelemetry> = HashMap::new();
    for (source, source_stats) in &stats.by_source {
        let processed = source_stats.success
            + source_stats.failed
            + source_stats.merged
            + source_stats.closed
            + source_stats.cannot_fix;
        let pending = source_stats.total.saturating_sub(processed);
        by_source.insert(
            source.clone(),
            SourceTelemetry {
                source: source.clone(),
                total: source_stats.total as i64,
                pending: pending as i64,
                success: source_stats.success as i64,
                failed: source_stats.failed as i64,
                merged: source_stats.merged as i64,
                closed: source_stats.closed as i64,
                cannot_fix: source_stats.cannot_fix as i64,
                retryable: 0,
                success_rate: 0.0,
            },
        );
    }
    for attempt in &retryable {
        let entry = by_source
            .entry(attempt.source.clone())
            .or_insert_with(|| SourceTelemetry {
                source: attempt.source.clone(),
                ..SourceTelemetry::default()
            });
        entry.retryable += 1;
    }
    let mut source_breakdown: Vec<SourceTelemetry> = by_source
        .into_values()
        .map(|mut s| {
            let processed = s.success + s.merged + s.failed + s.closed + s.cannot_fix;
            s.success_rate = if processed > 0 {
                ((s.success + s.merged) as f64 / processed as f64) * 100.0
            } else {
                0.0
            };
            s
        })
        .collect();
    source_breakdown.sort_by(|a, b| a.source.cmp(&b.source));

    let diagnostics = state
        .tracker
        .as_any()
        .downcast_ref::<SqliteTracker>()
        .and_then(|db| db.get_diagnostic_counts().ok());

    let twenty_four_h_ago_iso = (now - Duration::hours(24))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let seven_days_ago_iso = (now - Duration::days(7))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let agent_spawns_today = state
        .tracker
        .get_agent_spawn_count(&twenty_four_h_ago_iso)
        .unwrap_or(0);
    let agent_spawns_this_week = state
        .tracker
        .get_agent_spawn_count(&seven_days_ago_iso)
        .unwrap_or(0);

    let response = TelemetryOverviewResponse {
        generated_at: now.to_rfc3339(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        windows,
        queue,
        processing_time,
        source_breakdown,
        top_errors,
        activity_last_hour,
        metric_counts_last_24h,
        diagnostics,
        pr_analytics,
        agent_spawns_today,
        agent_spawns_this_week,
    };

    Ok(Json(response))
}

async fn telemetry_timeseries_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<TelemetryTimeseriesQuery>,
) -> Result<Json<TelemetryTimeseriesResponse>, StatusCode> {
    let now = Utc::now();
    let (period_label, period_duration) = parse_telemetry_period(query.period.as_deref(), "week");
    let default_bucket_minutes = match period_label.as_str() {
        "hour" => 5,
        "day" => 15,
        "month" => 360,
        _ => 60,
    };

    let bucket_minutes = query
        .bucket_minutes
        .unwrap_or(default_bucket_minutes)
        .clamp(1, 24 * 60);
    let bucket_seconds = bucket_minutes * 60;

    let start = now - period_duration;
    let mut buckets: BTreeMap<i64, TelemetryTimeseriesPoint> = BTreeMap::new();

    let mut cursor = floor_to_bucket(start, bucket_minutes);
    let end = floor_to_bucket(now, bucket_minutes);
    while cursor <= end {
        buckets.insert(
            cursor.timestamp(),
            TelemetryTimeseriesPoint {
                bucket_start: cursor.to_rfc3339(),
                ..TelemetryTimeseriesPoint::default()
            },
        );
        cursor += Duration::seconds(bucket_seconds);
    }

    for attempt in get_attempt_records_since(&state.tracker, start) {
        let bucket = floor_to_bucket(attempt.attempted_at, bucket_minutes).timestamp();
        if let Some(point) = buckets.get_mut(&bucket) {
            point.total += 1;
            match attempt.status {
                FixAttemptStatus::Pending => point.pending += 1,
                FixAttemptStatus::Success => point.success += 1,
                FixAttemptStatus::Failed => point.failed += 1,
                FixAttemptStatus::Merged => point.merged += 1,
                FixAttemptStatus::Closed => point.closed += 1,
                FixAttemptStatus::CannotFix => point.cannot_fix += 1,
            }
        }
    }

    Ok(Json(TelemetryTimeseriesResponse {
        period: period_label,
        bucket_minutes,
        generated_at: now.to_rfc3339(),
        points: buckets.into_values().collect(),
    }))
}

async fn telemetry_pipeline_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<TelemetryPeriodQuery>,
) -> Result<Json<TelemetryPipelineResponse>, StatusCode> {
    let now = Utc::now();
    let (period_label, period_duration) = parse_telemetry_period(query.period.as_deref(), "week");
    let since = now - period_duration;
    let total_metric_names = [
        "issues_fetched",
        "issues_matched",
        "issues_queued",
        "batch_processed",
        "pr_created",
        "ready_retries_found",
        "ready_retries_executed_total",
        "ready_retries_failed_total",
        "pr_status_checks",
        "pr_status_merged",
        "pr_status_closed",
        "pr_status_errors",
        "regression_watches_created",
        "auto_resolved_on_merge",
        "cascade_triggered",
        "cascade_failed",
    ];
    let per_source_metric_names = [
        "issues_fetched",
        "issues_matched",
        "issues_queued",
        "batch_processed",
        "pr_created",
        "ready_retry_executed",
        "ready_retry_failed",
    ];

    let mut totals = TelemetryPipelineTotals::default();
    let mut per_source: HashMap<String, TelemetryPipelineSource> = HashMap::new();

    if let Some(db) = state.tracker.as_any().downcast_ref::<SqliteTracker>() {
        let sums = db
            .get_metric_sums_since(&total_metric_names, since)
            .unwrap_or_default();

        totals.fetched = sums.get("issues_fetched").copied().unwrap_or(0.0);
        totals.matched = sums.get("issues_matched").copied().unwrap_or(0.0);
        totals.queued = sums.get("issues_queued").copied().unwrap_or(0.0);
        totals.processed = sums.get("batch_processed").copied().unwrap_or(0.0);
        totals.pr_created = sums.get("pr_created").copied().unwrap_or(0.0);
        totals.retries_found = sums.get("ready_retries_found").copied().unwrap_or(0.0);
        totals.retries_executed = sums
            .get("ready_retries_executed_total")
            .copied()
            .unwrap_or(0.0);
        totals.retries_failed = sums
            .get("ready_retries_failed_total")
            .copied()
            .unwrap_or(0.0);
        totals.pr_status_checks = sums.get("pr_status_checks").copied().unwrap_or(0.0);
        totals.pr_status_merged = sums.get("pr_status_merged").copied().unwrap_or(0.0);
        totals.pr_status_closed = sums.get("pr_status_closed").copied().unwrap_or(0.0);
        totals.pr_status_errors = sums.get("pr_status_errors").copied().unwrap_or(0.0);
        totals.regression_watches_created = sums
            .get("regression_watches_created")
            .copied()
            .unwrap_or(0.0);
        totals.auto_resolved_on_merge = sums.get("auto_resolved_on_merge").copied().unwrap_or(0.0);
        totals.cascade_triggered = sums.get("cascade_triggered").copied().unwrap_or(0.0);
        totals.cascade_failed = sums.get("cascade_failed").copied().unwrap_or(0.0);

        let per_source_sums = db
            .get_metric_sums_by_source_since(&per_source_metric_names, since)
            .unwrap_or_default();
        for ((metric_name, source), value) in per_source_sums {
            let entry =
                per_source
                    .entry(source.clone())
                    .or_insert_with(|| TelemetryPipelineSource {
                        source,
                        ..TelemetryPipelineSource::default()
                    });
            match metric_name.as_str() {
                "issues_fetched" => entry.fetched += value,
                "issues_matched" => entry.matched += value,
                "issues_queued" => entry.queued += value,
                "batch_processed" => entry.processed += value,
                "pr_created" => entry.pr_created += value,
                "ready_retry_executed" => entry.retries_executed += value,
                "ready_retry_failed" => entry.retries_failed += value,
                _ => {}
            }
        }
    } else {
        let issues_fetched = state
            .tracker
            .get_metrics("issues_fetched", Some(since), 50_000)
            .unwrap_or_default();
        let issues_matched = state
            .tracker
            .get_metrics("issues_matched", Some(since), 50_000)
            .unwrap_or_default();
        let issues_queued = state
            .tracker
            .get_metrics("issues_queued", Some(since), 50_000)
            .unwrap_or_default();
        let batch_processed = state
            .tracker
            .get_metrics("batch_processed", Some(since), 50_000)
            .unwrap_or_default();
        let pr_created = state
            .tracker
            .get_metrics("pr_created", Some(since), 50_000)
            .unwrap_or_default();
        let retries_found = state
            .tracker
            .get_metrics("ready_retries_found", Some(since), 50_000)
            .unwrap_or_default();
        let retries_executed = state
            .tracker
            .get_metrics("ready_retries_executed_total", Some(since), 50_000)
            .unwrap_or_default();
        let retries_failed = state
            .tracker
            .get_metrics("ready_retries_failed_total", Some(since), 50_000)
            .unwrap_or_default();
        let pr_status_checks = state
            .tracker
            .get_metrics("pr_status_checks", Some(since), 50_000)
            .unwrap_or_default();
        let pr_status_merged = state
            .tracker
            .get_metrics("pr_status_merged", Some(since), 50_000)
            .unwrap_or_default();
        let pr_status_closed = state
            .tracker
            .get_metrics("pr_status_closed", Some(since), 50_000)
            .unwrap_or_default();
        let pr_status_errors = state
            .tracker
            .get_metrics("pr_status_errors", Some(since), 50_000)
            .unwrap_or_default();
        let regression_watches_created = state
            .tracker
            .get_metrics("regression_watches_created", Some(since), 50_000)
            .unwrap_or_default();
        let auto_resolved_on_merge = state
            .tracker
            .get_metrics("auto_resolved_on_merge", Some(since), 50_000)
            .unwrap_or_default();
        let cascade_triggered = state
            .tracker
            .get_metrics("cascade_triggered", Some(since), 50_000)
            .unwrap_or_default();
        let cascade_failed = state
            .tracker
            .get_metrics("cascade_failed", Some(since), 50_000)
            .unwrap_or_default();

        totals = TelemetryPipelineTotals {
            fetched: sum_metric_values(&issues_fetched),
            matched: sum_metric_values(&issues_matched),
            queued: sum_metric_values(&issues_queued),
            processed: sum_metric_values(&batch_processed),
            pr_created: sum_metric_values(&pr_created),
            retries_found: sum_metric_values(&retries_found),
            retries_executed: sum_metric_values(&retries_executed),
            retries_failed: sum_metric_values(&retries_failed),
            pr_status_checks: sum_metric_values(&pr_status_checks),
            pr_status_merged: sum_metric_values(&pr_status_merged),
            pr_status_closed: sum_metric_values(&pr_status_closed),
            pr_status_errors: sum_metric_values(&pr_status_errors),
            regression_watches_created: sum_metric_values(&regression_watches_created),
            auto_resolved_on_merge: sum_metric_values(&auto_resolved_on_merge),
            cascade_triggered: sum_metric_values(&cascade_triggered),
            cascade_failed: sum_metric_values(&cascade_failed),
        };

        let per_source_retry_executed = state
            .tracker
            .get_metrics("ready_retry_executed", Some(since), 50_000)
            .unwrap_or_default();
        let per_source_retry_failed = state
            .tracker
            .get_metrics("ready_retry_failed", Some(since), 50_000)
            .unwrap_or_default();

        for metric in &issues_fetched {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.fetched += metric.metric_value;
            }
        }
        for metric in &issues_matched {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.matched += metric.metric_value;
            }
        }
        for metric in &issues_queued {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.queued += metric.metric_value;
            }
        }
        for metric in &batch_processed {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.processed += metric.metric_value;
            }
        }
        for metric in &pr_created {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.pr_created += metric.metric_value;
            }
        }
        for metric in &per_source_retry_executed {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.retries_executed += metric.metric_value;
            }
        }
        for metric in &per_source_retry_failed {
            if let Some(source) = &metric.source {
                let entry =
                    per_source
                        .entry(source.clone())
                        .or_insert_with(|| TelemetryPipelineSource {
                            source: source.clone(),
                            ..TelemetryPipelineSource::default()
                        });
                entry.retries_failed += metric.metric_value;
            }
        }
    }

    let poll_cycle_duration = state
        .tracker
        .get_metrics("poll_cycle_duration_secs", Some(since), 50_000)
        .unwrap_or_default();
    let active_processing = state
        .tracker
        .get_metrics("active_processing", Some(since), 50_000)
        .unwrap_or_default();
    let pending_attempts = state
        .tracker
        .get_metrics("pending_attempts", Some(since), 50_000)
        .unwrap_or_default();
    let total_attempts = state
        .tracker
        .get_metrics("total_attempts", Some(since), 50_000)
        .unwrap_or_default();

    let conversion = TelemetryPipelineConversion {
        match_rate: ratio(totals.matched, totals.fetched),
        queue_rate: ratio(totals.queued, totals.matched),
        processing_rate: ratio(totals.processed, totals.queued),
        pr_yield_rate: ratio(totals.pr_created, totals.processed),
    };

    let cycle_summary = compute_processing_time_summary(poll_cycle_duration);
    let poll_load = TelemetryPollLoad {
        poll_cycles: cycle_summary.samples,
        avg_cycle_secs: cycle_summary.avg_secs,
        p95_cycle_secs: cycle_summary.p95_secs,
        active_avg: average_metric_value(&active_processing),
        active_max: max_metric_value(&active_processing),
        pending_avg: average_metric_value(&pending_attempts),
        pending_max: max_metric_value(&pending_attempts),
        total_latest: latest_metric_value(&total_attempts),
    };

    let mut per_source: Vec<TelemetryPipelineSource> = per_source
        .into_values()
        .map(|mut src| {
            src.match_rate = ratio(src.matched, src.fetched);
            src.queue_rate = ratio(src.queued, src.matched);
            src.processing_rate = ratio(src.processed, src.queued);
            src.pr_yield_rate = ratio(src.pr_created, src.processed);
            src
        })
        .collect();
    per_source.sort_by(|a, b| a.source.cmp(&b.source));

    Ok(Json(TelemetryPipelineResponse {
        generated_at: now.to_rfc3339(),
        period: period_label,
        totals,
        conversion,
        poll_load,
        per_source,
    }))
}

async fn telemetry_latency_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
    Query(query): Query<TelemetryPeriodQuery>,
) -> Result<Json<TelemetryLatencyResponse>, StatusCode> {
    let now = Utc::now();
    let (period_label, period_duration) = parse_telemetry_period(query.period.as_deref(), "week");
    let since = now - period_duration;

    let processing_metrics = state
        .tracker
        .get_metrics("processing_time", Some(since), 50_000)
        .unwrap_or_default();
    let overall = compute_processing_time_summary(processing_metrics.clone());

    let all_values: Vec<f64> = processing_metrics.iter().map(|m| m.metric_value).collect();

    let mut by_status_values: HashMap<String, Vec<f64>> = HashMap::new();
    for metric in processing_metrics {
        let status = metric
            .tags
            .as_ref()
            .and_then(|tags| tags.get("status"))
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        by_status_values
            .entry(status)
            .or_default()
            .push(metric.metric_value);
    }

    let mut by_status: Vec<TelemetryLatencyByStatus> = by_status_values
        .into_iter()
        .map(|(status, values)| TelemetryLatencyByStatus {
            status,
            summary: compute_processing_value_summary(values),
        })
        .collect();
    by_status.sort_by(|a, b| {
        b.summary
            .samples
            .cmp(&a.summary.samples)
            .then_with(|| a.status.cmp(&b.status))
    });

    let bucket_defs: [(f64, &str); 5] = [
        (15.0, "<=15s"),
        (30.0, "<=30s"),
        (60.0, "<=60s"),
        (120.0, "<=2m"),
        (300.0, "<=5m"),
    ];
    let mut counts = vec![0i64; bucket_defs.len() + 1];
    for value in all_values {
        let mut placed = false;
        for (idx, (upper, _label)) in bucket_defs.iter().enumerate() {
            if value <= *upper {
                counts[idx] += 1;
                placed = true;
                break;
            }
        }
        if !placed {
            counts[bucket_defs.len()] += 1;
        }
    }

    let mut histogram = Vec::with_capacity(bucket_defs.len() + 1);
    for (idx, (upper, label)) in bucket_defs.iter().enumerate() {
        histogram.push(TelemetryLatencyHistogramBucket {
            label: (*label).to_string(),
            upper_bound_secs: Some(*upper),
            count: counts[idx],
        });
    }
    histogram.push(TelemetryLatencyHistogramBucket {
        label: ">5m".to_string(),
        upper_bound_secs: None,
        count: counts[bucket_defs.len()],
    });

    Ok(Json(TelemetryLatencyResponse {
        generated_at: now.to_rfc3339(),
        period: period_label,
        overall,
        by_status,
        histogram,
    }))
}

// ─── Config handlers ──────────────────────────────────

#[derive(Serialize)]
struct ConfigResponse {
    content: String,
}

#[derive(Deserialize)]
struct ConfigUpdateRequest {
    content: String,
}

/// Redact values of keys that look like secrets in raw TOML content.
fn redact_secrets(content: &str) -> String {
    let re = regex_lite::Regex::new(
        r#"(?im)^(\s*(?:[a-z_]*(?:token|secret|password|api_key|auth)[a-z_]*)\s*=\s*)("[^"]*"|'[^']*'|[^\n]*)"#,
    )
    .expect("valid regex");
    re.replace_all(content, r#"${1}"[REDACTED]""#).into_owned()
}

/// GET /api/config — return the raw TOML config file content with secrets redacted.
async fn get_config_handler(
    _user: AdminUser,
    State(state): State<ApiState>,
) -> Result<Json<ConfigResponse>, (StatusCode, Json<serde_json::Value>)> {
    let content = tokio::fs::read_to_string(&state.config_path)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to read config: {}", e) })),
            )
        })?;

    Ok(Json(ConfigResponse {
        content: redact_secrets(&content),
    }))
}

/// PUT /api/config — validate and write the TOML config file.
async fn put_config_handler(
    _user: AdminUser,
    State(state): State<ApiState>,
    Json(body): Json<ConfigUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let parsed = toml::from_str::<Config>(&body.content).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid TOML config: {}", e) })),
        )
    })?;

    parsed.validate().map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Config validation failed: {}", e) })),
        )
    })?;

    // Re-serialize the validated config to ensure only parsed fields are written to disk.
    // This prevents injection of arbitrary content via raw user input.
    let re_serialized = toml::to_string_pretty(&parsed).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to serialize config: {}", e) })),
        )
    })?;

    tokio::fs::write(&state.config_path, &re_serialized)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("Failed to write config: {}", e) })),
            )
        })?;

    Ok(Json(
        serde_json::json!({ "ok": true, "message": "Config saved. Restart to apply changes." }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AskConfig, CascadeConfig, ClaudeConfig, CodeIndexConfig, DiscordConfig, EmailConfig,
        GitHubAppConfig, GitHubConfig, LearningConfig, PrioritisationConfig, PushConfig,
        RegressionConfig, RetryConfig, SlackConfig, SmsConfig,
    };
    use crate::storage::{IndexingProgress, SqliteTracker};
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
            slack: SlackConfig::default(),
            email: EmailConfig::default(),
            sms: SmsConfig::default(),
            push: PushConfig::default(),
            ask: AskConfig::default(),
            github: GitHubConfig::default(),
            github_app: GitHubAppConfig::default(),
            retry: RetryConfig::default(),
            linear: None,
            sentry: None,
            jira: None,
            gitlab: None,
            regression: RegressionConfig::default(),
            cascade: CascadeConfig::default(),
            users: std::collections::HashMap::new(),
            learning: LearningConfig::default(),
            prioritisation: PrioritisationConfig::default(),
            code_index: CodeIndexConfig::default(),
            storage_dir: "/tmp/claudear-storage".into(),
            dashboard: crate::config::DashboardConfig::default(),
        }
    }

    fn create_test_tracker() -> Arc<dyn FixAttemptTracker> {
        Arc::new(SqliteTracker::in_memory().unwrap())
    }

    fn test_indexing_rx(
        tracker: &Arc<dyn FixAttemptTracker>,
    ) -> tokio::sync::watch::Receiver<IndexingProgress> {
        tracker.subscribe_indexing_progress()
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

        let indexing_rx = test_indexing_rx(tracker);
        let router = create_api_router(
            config,
            tracker.clone(),
            PathBuf::from("claudear.toml"),
            indexing_rx,
        )
        .layer(CookieManagerLayer::new());

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

    #[tokio::test]
    async fn test_telemetry_overview_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/telemetry/overview", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_telemetry_timeseries_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get(
                "/api/telemetry/timeseries?period=day&bucket_minutes=30",
                &token,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_telemetry_pipeline_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/telemetry/pipeline?period=day", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_telemetry_latency_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/telemetry/latency?period=week", &token))
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
            time_savings: None,
            agent_spawns_today: 0,
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

    #[tokio::test]
    async fn test_unauthenticated_returns_401() {
        let tracker = create_test_tracker();
        let config = test_config();
        let indexing_rx = test_indexing_rx(&tracker);
        let router =
            create_api_router(config, tracker, PathBuf::from("claudear.toml"), indexing_rx)
                .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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

    // ─── New integration tests for uncovered handlers ──────────────────────────

    #[tokio::test]
    async fn test_activity_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/activity", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let entries: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn test_activity_endpoint_with_limit() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/activity?limit=5", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_activity_endpoint_with_source_filter() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/activity?source=linear", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_analytics_summary_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/analytics/summary", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let summary: crate::types::AnalyticsSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary.total_processed, 0);
        assert_eq!(summary.total_successful, 0);
        assert_eq!(summary.total_merged, 0);
    }

    #[tokio::test]
    async fn test_metrics_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/metrics", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let metrics: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(metrics.is_empty());
    }

    #[tokio::test]
    async fn test_metrics_endpoint_with_name_filter() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get(
                "/api/metrics?name=processing_time&period=day&limit=10",
                &token,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_errors_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/errors", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let errors: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(errors.is_empty());
    }

    #[tokio::test]
    async fn test_errors_endpoint_with_limit() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/errors?limit=10", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_prs_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router.oneshot(auth_get("/api/prs", &token)).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let prs: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(prs.is_empty());
    }

    #[tokio::test]
    async fn test_prs_endpoint_with_status_filter() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/prs?status=open&limit=5", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_pr_analytics_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/prs/analytics", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let analytics: crate::types::PrAnalytics = serde_json::from_slice(&body).unwrap();
        assert_eq!(analytics.open, 0);
    }

    #[tokio::test]
    async fn test_feedback_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/feedback", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let feedback: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(feedback.is_empty());
    }

    #[tokio::test]
    async fn test_feedback_endpoint_with_filters() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/feedback?source=linear&limit=10", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_regressions_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/regressions", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let regressions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(regressions.is_empty());
    }

    #[tokio::test]
    async fn test_regressions_endpoint_with_status_filter() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/regressions?status=awaiting_release", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_regressions_endpoint_invalid_status() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/regressions?status=invalid_status", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_regression_checks_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/regressions/1/checks", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let checks: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(checks.is_empty());
    }

    #[tokio::test]
    async fn test_experiments_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/experiments", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let experiments: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(experiments.is_empty());
    }

    #[tokio::test]
    async fn test_repos_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/repos", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let repos: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(repos.is_empty());
    }

    #[tokio::test]
    async fn test_repo_stats_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/repos/stats", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dependencies_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/repos/dependencies", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let deps: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(deps.is_empty());
    }

    #[tokio::test]
    async fn test_inference_stats_endpoint() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/inference/stats", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_inference_history_endpoint_empty() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/inference/history", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let history: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn test_inference_history_endpoint_with_limit() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/inference/history?limit=10", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_config_endpoint_admin() {
        let tracker = create_test_tracker();
        let config = test_config();

        // Use a nonexistent config path to test the error case
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("testpass", 4).unwrap();
        db.create_user("admin@test.com", &password_hash, "Admin", "admin")
            .unwrap();
        let token = db.create_session(1, "2099-12-31 23:59:59").unwrap();

        let indexing_rx = test_indexing_rx(&tracker);
        let router = create_api_router(
            config,
            tracker.clone(),
            PathBuf::from("/tmp/nonexistent_claudear_test_config.toml"),
            indexing_rx,
        )
        .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(auth_get("/api/config", &token))
            .await
            .unwrap();

        // Config file doesn't exist on disk, so expect 500
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_get_config_endpoint_admin_success() {
        let tracker = create_test_tracker();
        let config = test_config();

        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("testpass", 4).unwrap();
        db.create_user("admin@test.com", &password_hash, "Admin", "admin")
            .unwrap();
        let token = db.create_session(1, "2099-12-31 23:59:59").unwrap();

        // Write a temp config file
        let config_path = std::env::temp_dir().join("claudear_test_config.toml");
        std::fs::write(&config_path, "# test config\nwork_dir = \"/tmp\"\n").unwrap();

        let indexing_rx = test_indexing_rx(&tracker);
        let router = create_api_router(config, tracker.clone(), config_path.clone(), indexing_rx)
            .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(auth_get("/api/config", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(resp["content"].as_str().unwrap().contains("work_dir"));
        assert!(resp["path"].is_null(), "path field should not be exposed");

        // Cleanup
        let _ = std::fs::remove_file(&config_path);
    }

    #[tokio::test]
    async fn test_get_config_endpoint_viewer_forbidden() {
        let tracker = create_test_tracker();
        let config = test_config();
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("testpass", 4).unwrap();
        db.create_user("viewer@test.com", &password_hash, "Viewer", "viewer")
            .unwrap();
        let token = db.create_session(1, "2099-12-31 23:59:59").unwrap();

        let indexing_rx = test_indexing_rx(&tracker);
        let router = create_api_router(
            config,
            tracker.clone(),
            PathBuf::from("claudear.toml"),
            indexing_rx,
        )
        .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(auth_get("/api/config", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_put_config_endpoint_invalid_toml() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let request = Request::builder()
            .method("PUT")
            .uri("/api/config")
            .header("cookie", format!("claudear_session={}", token))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "content": "this is not valid toml [[[["
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_put_config_endpoint_viewer_forbidden() {
        let tracker = create_test_tracker();
        let config = test_config();
        let db = tracker.as_any().downcast_ref::<SqliteTracker>().unwrap();
        let password_hash = bcrypt::hash("testpass", 4).unwrap();
        db.create_user("viewer@test.com", &password_hash, "Viewer", "viewer")
            .unwrap();
        let token = db.create_session(1, "2099-12-31 23:59:59").unwrap();

        let indexing_rx = test_indexing_rx(&tracker);
        let router = create_api_router(
            config,
            tracker.clone(),
            PathBuf::from("claudear.toml"),
            indexing_rx,
        )
        .layer(CookieManagerLayer::new());

        let request = Request::builder()
            .method("PUT")
            .uri("/api/config")
            .header("cookie", format!("claudear_session={}", token))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_string(&serde_json::json!({
                    "content": "key = \"value\""
                }))
                .unwrap(),
            ))
            .unwrap();

        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_attempt_full_detail_not_found() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts/99999/detail", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_attempt_execution_log_not_found() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // Attempt doesn't exist
        let response = router
            .oneshot(auth_get("/api/attempts/99999/logs/1/stdout", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_attempt_execution_log_invalid_stream() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/attempts/1/logs/1/invalid_stream", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // ─── Tests with seeded data ──────────────────────────────────

    /// Helper to seed a fix attempt and return the tracker.
    fn seed_attempt(
        tracker: &Arc<dyn FixAttemptTracker>,
        source: &str,
        issue_id: &str,
        short_id: &str,
    ) {
        tracker.record_attempt(source, issue_id, short_id).unwrap();
    }

    #[tokio::test]
    async fn test_stats_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "linear", "issue-2", "PROJ-2");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/stats", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let stats: FixAttemptStats = serde_json::from_slice(&body).unwrap();
        assert_eq!(stats.total, 2);
        assert_eq!(stats.success, 1);
        assert_eq!(stats.pending, 1);
    }

    #[tokio::test]
    async fn test_overview_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();
        tracker.mark_merged("linear", "issue-1").unwrap();

        seed_attempt(&tracker, "sentry", "issue-2", "SENTRY-1");
        tracker
            .mark_failed("sentry", "issue-2", "Build failed")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/stats/overview", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("success_rate"));
        assert!(body_str.contains("merge_rate"));
        assert!(body_str.contains("recent_attempts"));
        assert!(body_str.contains("sources"));
    }

    #[tokio::test]
    async fn test_attempts_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "sentry", "issue-2", "SENTRY-1");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/attempts", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"total\":2"));
    }

    #[tokio::test]
    async fn test_attempts_status_filter_with_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "linear", "issue-2", "PROJ-2");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/attempts?status=success", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"total\":1"));
    }

    #[tokio::test]
    async fn test_attempts_source_filter_with_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "sentry", "issue-2", "SENTRY-1");

        let response = router
            .oneshot(auth_get("/api/attempts?source=sentry", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"total\":1"));
    }

    #[tokio::test]
    async fn test_attempt_detail_with_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");

        let response = router
            .oneshot(auth_get("/api/attempts/1", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let attempt: FixAttempt = serde_json::from_slice(&body).unwrap();
        assert_eq!(attempt.source, "linear");
        assert_eq!(attempt.short_id, "PROJ-1");
        assert_eq!(attempt.status, FixAttemptStatus::Pending);
    }

    #[tokio::test]
    async fn test_attempt_full_detail_with_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();

        // Use attempt ID 1 (assuming it's the first one recorded).
        // Note: the ID might be 2 if the user creation in create_authenticated_router
        // interferes, but record_attempt in the fix_attempts table is separate.
        let response = router
            .oneshot(auth_get("/api/attempts/1/detail", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("\"attempt\""));
        assert!(body_str.contains("\"executions\""));
        assert!(body_str.contains("\"reviews\""));
    }

    #[tokio::test]
    async fn test_retries_with_seeded_failed_attempt() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        tracker
            .mark_failed("linear", "issue-1", "Build failed")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/retries", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("retryable"));
        assert!(body_str.contains("ready"));
        assert!(body_str.contains("max_retries"));
    }

    #[tokio::test]
    async fn test_activity_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let entry = crate::types::ActivityLogEntry::new("issue_received", "Received PROJ-1")
            .with_source("linear")
            .with_issue("issue-1", "PROJ-1");
        tracker.record_activity(&entry).unwrap();

        let response = router
            .oneshot(auth_get("/api/activity", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let entries: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["activity_type"], "issue_received");
        assert_eq!(entries[0]["message"], "Received PROJ-1");
    }

    #[tokio::test]
    async fn test_metrics_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let metric = crate::types::ProcessingMetric {
            id: 0,
            timestamp: chrono::Utc::now(),
            metric_name: "processing_time".to_string(),
            metric_value: 42.5,
            source: Some("linear".to_string()),
            tags: None,
        };
        tracker.record_metric(&metric).unwrap();

        let response = router
            .oneshot(auth_get("/api/metrics?name=processing_time", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let metrics: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0]["metric_name"], "processing_time");
    }

    #[tokio::test]
    async fn test_errors_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let error_pattern = crate::types::ErrorPattern {
            id: 0,
            pattern_hash: "abc123".to_string(),
            error_type: Some("build_failure".to_string()),
            error_message: Some("Failed to compile".to_string()),
            first_seen: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
            occurrence_count: 3,
            sources: Some(vec!["linear".to_string()]),
            example_issue_ids: None,
            resolution_hints: None,
        };
        tracker.record_error_pattern(&error_pattern).unwrap();

        let response = router
            .oneshot(auth_get("/api/errors", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let errors: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0]["pattern_hash"], "abc123");
    }

    #[tokio::test]
    async fn test_overview_rates_computation() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // Seed 4 attempts, 2 success, 1 merged, 1 failed
        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "linear", "issue-2", "PROJ-2");
        seed_attempt(&tracker, "linear", "issue-3", "PROJ-3");
        seed_attempt(&tracker, "linear", "issue-4", "PROJ-4");

        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();
        tracker
            .mark_success("linear", "issue-2", "https://github.com/org/repo/pull/2")
            .unwrap();
        tracker.mark_merged("linear", "issue-2").unwrap();
        tracker
            .mark_failed("linear", "issue-3", "Build failed")
            .unwrap();

        let response = router
            .oneshot(auth_get("/api/stats/overview", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let overview: serde_json::Value = serde_json::from_slice(&body).unwrap();

        // success_rate = (success + merged) / total * 100 = (1 + 1) / 4 * 100 = 50.0
        let success_rate = overview["success_rate"].as_f64().unwrap();
        assert!(success_rate > 0.0);

        // stats.total should be 4
        assert_eq!(overview["stats"]["total"], 4);
    }

    #[tokio::test]
    async fn test_sources_response_structure() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/sources", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        // Default config has no sources configured, so sources array should be empty
        assert!(body_str.contains("\"sources\":[]"));
    }

    #[tokio::test]
    async fn test_health_reports_database_ok() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/health", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let health: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(health["status"], "ok");
        assert_eq!(health["database"]["status"], "ok");
        assert!(health["uptime_secs"].as_u64().is_some());
        assert!(health["version"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_attempts_pagination_page_2() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // Seed 3 attempts
        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        seed_attempt(&tracker, "linear", "issue-2", "PROJ-2");
        seed_attempt(&tracker, "linear", "issue-3", "PROJ-3");

        let response = router
            .oneshot(auth_get("/api/attempts?page=2&per_page=2", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["page"], 2);
        assert_eq!(resp["per_page"], 2);
        assert_eq!(resp["total"], 3);
        // Page 2 with per_page=2 should have 1 result
        let attempts = resp["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
    }

    #[tokio::test]
    async fn test_unauthenticated_activity_returns_401() {
        let tracker = create_test_tracker();
        let config = test_config();
        let indexing_rx = test_indexing_rx(&tracker);
        let router =
            create_api_router(config, tracker, PathBuf::from("claudear.toml"), indexing_rx)
                .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/activity")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_unauthenticated_config_returns_401() {
        let tracker = create_test_tracker();
        let config = test_config();
        let indexing_rx = test_indexing_rx(&tracker);
        let router =
            create_api_router(config, tracker, PathBuf::from("claudear.toml"), indexing_rx)
                .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_unauthenticated_experiments_returns_401() {
        let tracker = create_test_tracker();
        let config = test_config();
        let indexing_rx = test_indexing_rx(&tracker);
        let router =
            create_api_router(config, tracker, PathBuf::from("claudear.toml"), indexing_rx)
                .layer(CookieManagerLayer::new());

        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/experiments")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_analytics_summary_with_seeded_data() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        seed_attempt(&tracker, "linear", "issue-1", "PROJ-1");
        tracker
            .mark_success("linear", "issue-1", "https://github.com/org/repo/pull/1")
            .unwrap();
        tracker.mark_merged("linear", "issue-1").unwrap();

        let response = router
            .oneshot(auth_get("/api/analytics/summary", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let summary: crate::types::AnalyticsSummary = serde_json::from_slice(&body).unwrap();
        assert_eq!(summary.total_merged, 1);
    }

    #[tokio::test]
    async fn test_telemetry_timeseries_with_different_periods() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // Test hour period
        let response = router
            .oneshot(auth_get(
                "/api/telemetry/timeseries?period=hour&bucket_minutes=5",
                &token,
            ))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["period"], "hour");
        assert_eq!(resp["bucket_minutes"], 5);
        assert!(resp["points"].as_array().unwrap().len() > 0);
    }

    #[tokio::test]
    async fn test_telemetry_pipeline_with_different_periods() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        let response = router
            .oneshot(auth_get("/api/telemetry/pipeline?period=month", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["period"], "month");
        assert!(resp["totals"].is_object());
        assert!(resp["conversion"].is_object());
        assert!(resp["poll_load"].is_object());
    }

    #[tokio::test]
    async fn test_telemetry_latency_default_period() {
        let tracker = create_test_tracker();
        let (router, token) = create_authenticated_router(&tracker);

        // No period parameter - should default to "week"
        let response = router
            .oneshot(auth_get("/api/telemetry/latency", &token))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(resp["period"], "week");
        assert!(resp["overall"].is_object());
        assert!(resp["histogram"].as_array().unwrap().len() > 0);
    }

    // ─── Utility function tests ──────────────────────────────────

    #[test]
    fn test_tail_utf8_short_string() {
        let (result, truncated) = tail_utf8("hello", 100);
        assert_eq!(result, "hello");
        assert!(!truncated);
    }

    #[test]
    fn test_tail_utf8_long_string() {
        let long = "a".repeat(1000);
        let (result, truncated) = tail_utf8(&long, 100);
        assert!(truncated);
        assert!(result.contains("...[truncated]"));
        // The tail should contain at most ~100 bytes of the original plus the prefix
        assert!(result.len() <= 200);
    }

    #[test]
    fn test_parse_telemetry_period_known() {
        let (label, _duration) = parse_telemetry_period(Some("hour"), "week");
        assert_eq!(label, "hour");

        let (label, _duration) = parse_telemetry_period(Some("day"), "week");
        assert_eq!(label, "day");

        let (label, _duration) = parse_telemetry_period(Some("month"), "week");
        assert_eq!(label, "month");

        let (label, _duration) = parse_telemetry_period(Some("week"), "day");
        assert_eq!(label, "week");
    }

    #[test]
    fn test_parse_telemetry_period_unknown_falls_back() {
        let (label, _duration) = parse_telemetry_period(Some("year"), "day");
        assert_eq!(label, "day");

        let (label, _duration) = parse_telemetry_period(None, "hour");
        assert_eq!(label, "hour");
    }

    #[test]
    fn test_floor_to_bucket() {
        use chrono::{TimeZone, Timelike};
        let ts = chrono::Utc
            .with_ymd_and_hms(2024, 6, 15, 10, 37, 42)
            .unwrap();

        // Floor to 15-minute buckets
        let floored = floor_to_bucket(ts, 15);
        assert_eq!(floored.minute(), 30);
        assert_eq!(floored.second(), 0);

        // Floor to 60-minute buckets
        let floored = floor_to_bucket(ts, 60);
        assert_eq!(floored.minute(), 0);
        assert_eq!(floored.second(), 0);
    }

    #[test]
    fn test_compute_processing_value_summary_empty() {
        let summary = compute_processing_value_summary(vec![]);
        assert_eq!(summary.samples, 0);
        assert!(summary.avg_secs.is_none());
        assert!(summary.max_secs.is_none());
    }

    #[test]
    fn test_compute_processing_value_summary_with_data() {
        let summary = compute_processing_value_summary(vec![10.0, 20.0, 30.0, 40.0, 50.0]);
        assert_eq!(summary.samples, 5);
        assert!((summary.avg_secs.unwrap() - 30.0).abs() < 0.01);
        assert!((summary.max_secs.unwrap() - 50.0).abs() < 0.01);
        assert!(summary.p50_secs.is_some());
        assert!(summary.p95_secs.is_some());
        assert!(summary.p99_secs.is_some());
    }

    #[test]
    fn test_ratio_function() {
        assert!((ratio(50.0, 100.0).unwrap() - 0.5).abs() < 0.001);
        assert!(ratio(10.0, 0.0).is_none());
    }

    #[test]
    fn test_summarize_window_empty() {
        let attempts: Vec<FixAttempt> = vec![];
        let metric = summarize_window(&attempts, "1h", chrono::Utc::now() - Duration::hours(1));
        assert_eq!(metric.window, "1h");
        assert_eq!(metric.processed, 0);
        assert_eq!(metric.successful, 0);
        assert_eq!(metric.failed, 0);
    }

    #[test]
    fn test_summarize_window_with_attempts() {
        let now = chrono::Utc::now();
        let attempts = vec![
            FixAttempt {
                id: 1,
                source: "linear".to_string(),
                issue_id: "1".to_string(),
                short_id: "P-1".to_string(),
                status: FixAttemptStatus::Success,
                pr_url: None,
                github_repo: None,
                github_pr_number: None,
                error_message: None,
                attempted_at: now,
                resolved_at: None,
                merged_at: None,
                retry_count: 0,
                last_retry_at: None,
                issue_labels: vec![],
                parent_attempt_id: None,
                cascade_repo: None,
            },
            FixAttempt {
                id: 2,
                source: "linear".to_string(),
                issue_id: "2".to_string(),
                short_id: "P-2".to_string(),
                status: FixAttemptStatus::Failed,
                pr_url: None,
                github_repo: None,
                github_pr_number: None,
                error_message: Some("error".to_string()),
                attempted_at: now,
                resolved_at: None,
                merged_at: None,
                retry_count: 0,
                last_retry_at: None,
                issue_labels: vec![],
                parent_attempt_id: None,
                cascade_repo: None,
            },
        ];
        let metric = summarize_window(&attempts, "24h", now - Duration::hours(24));
        assert_eq!(metric.window, "24h");
        assert_eq!(metric.processed, 2);
        assert_eq!(metric.successful, 1);
        assert_eq!(metric.failed, 1);
        assert!((metric.success_rate - 50.0).abs() < 0.01);
    }
}

//! API route handlers for the dashboard.

use super::auth::*;
use crate::config::Config;
use crate::retry::RetryManager;
use crate::storage::FixAttemptTracker;
use crate::types::{FixAttempt, FixAttemptStats, FixAttemptStatus, RegressionWatchStatus};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
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
    let page = query.page.unwrap_or(1).max(1);
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

    let execution = state
        .tracker
        .get_executions_for_attempt(attempt_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .find(|e| e.id == execution_id)
        .ok_or(StatusCode::NOT_FOUND)?;

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

async fn telemetry_overview_handler(
    _user: AuthUser,
    State(state): State<ApiState>,
) -> Result<Json<TelemetryOverviewResponse>, StatusCode> {
    let now = Utc::now();
    let attempts = get_attempt_records(&state.tracker);

    let windows = vec![
        summarize_window(&attempts, "1h", now - Duration::hours(1)),
        summarize_window(&attempts, "24h", now - Duration::hours(24)),
        summarize_window(&attempts, "7d", now - Duration::days(7)),
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

    let open_prs = state
        .tracker
        .get_open_prs()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let pr_analytics = state
        .tracker
        .get_pr_analytics()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let watches = state
        .tracker
        .get_all_regression_watches()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut queue = TelemetryQueueMetrics {
        pending_attempts: state
            .tracker
            .get_stats()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .pending as i64,
        retryable_attempts: retryable.len() as i64,
        ready_retries,
        open_prs: open_prs.len() as i64,
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

    let mut activity_last_hour: HashMap<String, i64> = HashMap::new();
    for entry in state
        .tracker
        .get_recent_activities_filtered(5_000, None)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.timestamp >= now - Duration::hours(1))
    {
        *activity_last_hour.entry(entry.activity_type).or_insert(0) += 1;
    }

    let mut metric_counts_last_24h: HashMap<String, i64> = HashMap::new();
    for metric_name in [
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
    ] {
        let count = state
            .tracker
            .get_metrics(metric_name, Some(now - Duration::hours(24)), 20_000)
            .map(|m| m.len() as i64)
            .unwrap_or(0);
        metric_counts_last_24h.insert(metric_name.to_string(), count);
    }

    let mut by_source: HashMap<String, SourceTelemetry> = HashMap::new();
    for attempt in &attempts {
        let entry = by_source
            .entry(attempt.source.clone())
            .or_insert_with(|| SourceTelemetry {
                source: attempt.source.clone(),
                ..SourceTelemetry::default()
            });
        entry.total += 1;
        match attempt.status {
            FixAttemptStatus::Pending => entry.pending += 1,
            FixAttemptStatus::Success => entry.success += 1,
            FixAttemptStatus::Failed => entry.failed += 1,
            FixAttemptStatus::Merged => entry.merged += 1,
            FixAttemptStatus::Closed => entry.closed += 1,
            FixAttemptStatus::CannotFix => entry.cannot_fix += 1,
        }
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
        .downcast_ref::<crate::storage::SqliteTracker>()
        .and_then(|db| db.get_diagnostic_counts().ok());

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

    for attempt in get_attempt_records(&state.tracker)
        .into_iter()
        .filter(|a| a.attempted_at >= start)
    {
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

    let totals = TelemetryPipelineTotals {
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

    let per_source_retry_executed = state
        .tracker
        .get_metrics("ready_retry_executed", Some(since), 50_000)
        .unwrap_or_default();
    let per_source_retry_failed = state
        .tracker
        .get_metrics("ready_retry_failed", Some(since), 50_000)
        .unwrap_or_default();

    let mut per_source: HashMap<String, TelemetryPipelineSource> = HashMap::new();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AskConfig, CascadeConfig, ClaudeConfig, DiscordConfig, EmailConfig, GitHubAppConfig,
        GitHubConfig, PushConfig, RegressionConfig, RetryConfig, SmsConfig,
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
            ask: AskConfig::default(),
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
        let router = create_api_router(config, tracker).layer(CookieManagerLayer::new());

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
}

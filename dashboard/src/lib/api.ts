const API_BASE = '/api';

// ─── Existing types ──────────────────────────────────

export interface Stats {
  total: number;
  pending: number;
  success: number;
  failed: number;
  merged: number;
  closed: number;
  cannot_fix: number;
  by_source: Record<string, SourceStats>;
}

export interface SourceStats {
  total: number;
  success: number;
  failed: number;
  merged: number;
  closed: number;
  cannot_fix: number;
}

export interface AttemptSummary {
  id: number;
  source: string;
  short_id: string;
  title: string;
  status: string;
  pr_url: string | null;
  attempted_at: string;
  retry_count: number;
}

export interface SourceSummary {
  name: string;
  total: number;
  success: number;
  failed: number;
  merged: number;
  success_rate: number;
}

export interface Overview {
  stats: Stats;
  success_rate: number;
  merge_rate: number;
  recent_attempts: AttemptSummary[];
  sources: SourceSummary[];
}

export interface AttemptsResponse {
  attempts: AttemptSummary[];
  total: number;
  page: number;
  per_page: number;
}

export interface Health {
  status: string;
  version: string;
  uptime_secs: number;
  database: {
    status: string;
    error?: string;
  };
}

export interface RetriesResponse {
  retryable: AttemptSummary[];
  ready: AttemptSummary[];
  max_retries: number;
}

export interface SourceInfo {
  name: string;
  enabled: boolean;
  config: Record<string, unknown>;
}

export interface SourcesResponse {
  sources: SourceInfo[];
}

// ─── Auth types ──────────────────────────────────

export interface AuthUser {
  id: number
  email: string
  name: string
  role: string
}

export interface LoginResponse {
  user: AuthUser
}

export interface UserRecord {
  id: number
  email: string
  name: string
  role: string
  created_at: string
  updated_at: string
}

// ─── New types ──────────────────────────────────

export interface ActivityLogEntry {
  id: number;
  timestamp: string;
  activity_type: string;
  source: string | null;
  issue_id: string | null;
  short_id: string | null;
  message: string;
  metadata: Record<string, unknown> | null;
}

export interface ClaudeExecution {
  id: number;
  attempt_id: number | null;
  started_at: string;
  completed_at: string | null;
  duration_secs: number | null;
  exit_code: number | null;
  timed_out: boolean;
  stdout_preview: string | null;
  stderr_preview: string | null;
  stdout_log_path: string | null;
  stderr_log_path: string | null;
  event_log_path?: string | null;
  prompt_used: string | null;
  prompt_hash: string | null;
  model_version: string | null;
  working_directory: string | null;
  git_branch: string | null;
  git_commit_before: string | null;
  git_commit_after: string | null;
  files_changed: number | null;
  lines_added: number | null;
  lines_removed: number | null;
}

export interface PrReviewRecord {
  id: number;
  attempt_id: number | null;
  pr_url: string;
  reviewer: string | null;
  review_state: string | null;
  submitted_at: string | null;
  body: string | null;
  sentiment: string | null;
  actionable_feedback: string | null;
}

export interface FixAttemptDetail {
  id: number;
  issue_id: string;
  short_id: string;
  source: string;
  attempted_at: string;
  pr_url: string | null;
  github_repo: string | null;
  github_pr_number: number | null;
  status: string;
  error_message: string | null;
  merged_at: string | null;
  resolved_at: string | null;
  retry_count: number;
  last_retry_at: string | null;
  issue_labels: string[];
  parent_attempt_id: number | null;
  cascade_repo: string | null;
}

export interface AttemptDetailResponse {
  attempt: FixAttemptDetail;
  executions: ClaudeExecution[];
  reviews: PrReviewRecord[];
  feedback: FixOutcome | null;
}

export interface AttemptExecutionLogResponse {
  attempt_id: number;
  execution_id: number;
  stream: 'stdout' | 'stderr' | 'events';
  path: string | null;
  content: string | null;
  truncated: boolean;
}

export interface AnalyticsSummary {
  success_rate: number;
  total_processed: number;
  total_successful: number;
  total_merged: number;
  avg_processing_time_secs: number | null;
  avg_time_to_merge_hours: number | null;
  most_common_error: string | null;
  success_rate_by_source: Record<string, number>;
}

export interface ProcessingMetric {
  id: number;
  timestamp: string;
  metric_name: string;
  metric_value: number;
  source: string | null;
  tags: Record<string, unknown> | null;
}

export interface ErrorPattern {
  id: number;
  pattern_hash: string;
  error_type: string | null;
  error_message: string | null;
  first_seen: string;
  last_seen: string;
  occurrence_count: number;
  sources: string[] | null;
  example_issue_ids: string[] | null;
  resolution_hints: string | null;
}

export interface PrRecord {
  id: number;
  pr_url: string;
  github_repo: string;
  pr_number: number;
  attempt_id: number | null;
  issue_id: string | null;
  issue_source: string | null;
  title: string | null;
  description: string | null;
  author: string | null;
  head_branch: string | null;
  base_branch: string | null;
  status: string;
  created_at: string;
  updated_at: string | null;
  merged_at: string | null;
  closed_at: string | null;
  approvals_count: number;
  changes_requested_count: number;
  comments_count: number;
  last_review_at: string | null;
  time_to_first_review_mins: number | null;
  time_to_merge_mins: number | null;
  review_cycles: number;
  files_changed: number | null;
  lines_added: number | null;
  lines_removed: number | null;
}

export interface PrAnalytics {
  total: number;
  open: number;
  merged: number;
  closed: number;
  avg_time_to_first_review_mins: number | null;
  avg_time_to_merge_mins: number | null;
  avg_review_cycles: number | null;
  merge_rate: number | null;
  by_repo: Record<string, number>;
}

export interface FixOutcome {
  id: number;
  attempt_id: number;
  source: string;
  issue_id: string;
  issue_text: string;
  prompt_used: string;
  outcome: string;
  error_type: string | null;
  learnings: string | null;
  keywords: string[];
}

export interface RegressionWatch {
  id: number;
  issue_type: string;
  issue_id: string;
  fix_attempt_id: number;
  status: string;
  pr_merged_at: string | null;
  monitoring_started_at: string | null;
  resolved_at: string | null;
  regressed_at: string | null;
  created_at: string;
}

export interface RegressionCheck {
  id: number;
  regression_watch_id: number;
  issue_still_exists: boolean;
  checked_at: string | null;
  check_details: string | null;
  created_at: string;
}

export interface PromptExperiment {
  id: number;
  experiment_name: string;
  variant: string;
  prompt_template: string;
  prompt_hash: string;
  created_at: string;
  active: boolean;
  success_count: number;
  failure_count: number;
  avg_time_to_merge: number | null;
  avg_review_score: number | null;
}

export interface StoredIndexedRepo {
  id: number;
  name: string;
  path: string;
  github_url: string | null;
  default_branch: string;
  file_count: number;
  last_indexed_at: string;
  created_at: string;
}

export interface IndexStats {
  repo_count: number;
  file_count: number;
  last_indexed_at: string | null;
}

export interface StoredDependency {
  id: number;
  upstream: string;
  downstream: string;
  dep_type: string;
  created_at: string;
}

export interface InferenceStats {
  total_attempts: number;
  with_feedback: number;
  correct: number;
  accuracy: number;
  by_confidence: {
    high: number;
    medium: number;
    low: number;
    none: number;
  };
}

export interface InferenceHistoryEntry {
  id: number;
  issue_id: string;
  issue_source: string;
  extracted_keywords: string | null;
  inferred_repo_name: string | null;
  confidence: string | null;
  inference_reason: string | null;
  was_correct: boolean | null;
  duration_ms: number | null;
  created_at: string;
}

export interface TelemetryWindowMetric {
  window: string;
  processed: number;
  successful: number;
  failed: number;
  merged: number;
  success_rate: number;
  error_rate: number;
  throughput_per_hour: number;
}

export interface TelemetryQueueMetrics {
  pending_attempts: number;
  retryable_attempts: number;
  ready_retries: number;
  open_prs: number;
  watches_awaiting_release: number;
  watches_monitoring: number;
  watches_resolved: number;
  watches_regressed: number;
}

export interface ProcessingTimeSummary {
  samples: number;
  avg_secs: number | null;
  p50_secs: number | null;
  p95_secs: number | null;
  p99_secs: number | null;
  max_secs: number | null;
}

export interface TelemetryProcessingTime {
  all_time: ProcessingTimeSummary;
  last_24h: ProcessingTimeSummary;
}

export interface SourceTelemetry {
  source: string;
  total: number;
  pending: number;
  success: number;
  failed: number;
  merged: number;
  closed: number;
  cannot_fix: number;
  retryable: number;
  success_rate: number;
}

export interface TelemetryTimeseriesPoint {
  bucket_start: string;
  total: number;
  pending: number;
  success: number;
  failed: number;
  merged: number;
  closed: number;
  cannot_fix: number;
}

export interface TelemetryOverview {
  generated_at: string;
  uptime_secs: number;
  windows: TelemetryWindowMetric[];
  queue: TelemetryQueueMetrics;
  processing_time: TelemetryProcessingTime;
  source_breakdown: SourceTelemetry[];
  top_errors: ErrorPattern[];
  activity_last_hour: Record<string, number>;
  metric_counts_last_24h: Record<string, number>;
  diagnostics?: Record<string, unknown> | null;
  pr_analytics: PrAnalytics;
}

export interface TelemetryTimeseries {
  period: string;
  bucket_minutes: number;
  generated_at: string;
  points: TelemetryTimeseriesPoint[];
}

export interface TelemetryPipelineTotals {
  fetched: number;
  matched: number;
  queued: number;
  processed: number;
  pr_created: number;
  retries_found: number;
  retries_executed: number;
  retries_failed: number;
  pr_status_checks: number;
  pr_status_merged: number;
  pr_status_closed: number;
  pr_status_errors: number;
  regression_watches_created: number;
  auto_resolved_on_merge: number;
  cascade_triggered: number;
  cascade_failed: number;
}

export interface TelemetryPipelineConversion {
  match_rate: number | null;
  queue_rate: number | null;
  processing_rate: number | null;
  pr_yield_rate: number | null;
}

export interface TelemetryPollLoad {
  poll_cycles: number;
  avg_cycle_secs: number | null;
  p95_cycle_secs: number | null;
  active_avg: number | null;
  active_max: number | null;
  pending_avg: number | null;
  pending_max: number | null;
  total_latest: number | null;
}

export interface TelemetryPipelineSource {
  source: string;
  fetched: number;
  matched: number;
  queued: number;
  processed: number;
  pr_created: number;
  retries_executed: number;
  retries_failed: number;
  match_rate: number | null;
  queue_rate: number | null;
  processing_rate: number | null;
  pr_yield_rate: number | null;
}

export interface TelemetryPipeline {
  generated_at: string;
  period: string;
  totals: TelemetryPipelineTotals;
  conversion: TelemetryPipelineConversion;
  poll_load: TelemetryPollLoad;
  per_source: TelemetryPipelineSource[];
}

export interface TelemetryLatencyByStatus {
  status: string;
  summary: ProcessingTimeSummary;
}

export interface TelemetryLatencyHistogramBucket {
  label: string;
  upper_bound_secs: number | null;
  count: number;
}

export interface TelemetryLatency {
  generated_at: string;
  period: string;
  overall: ProcessingTimeSummary;
  by_status: TelemetryLatencyByStatus[];
  histogram: TelemetryLatencyHistogramBucket[];
}

// ─── Fetchers ──────────────────────────────────

let onUnauthorized: (() => void) | null = null

export function setOnUnauthorized(cb: () => void) {
  onUnauthorized = cb
}

async function fetchJson<T>(url: string): Promise<T> {
  const res = await fetch(url)
  if (res.status === 401) {
    onUnauthorized?.()
    throw new Error('Unauthorized')
  }
  if (!res.ok) throw new Error(`Failed to fetch ${url}: ${res.status}`)
  return res.json()
}

async function postJson<T>(url: string, body?: unknown): Promise<T> {
  const res = await fetch(url, {
    method: 'POST',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  })
  if (res.status === 401) {
    onUnauthorized?.()
    throw new Error('Unauthorized')
  }
  if (!res.ok) throw new Error(`Failed to post ${url}: ${res.status}`)
  if (res.status === 204) return undefined as T
  return res.json()
}

async function putJson<T>(url: string, body: unknown): Promise<T> {
  const res = await fetch(url, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (res.status === 401) {
    onUnauthorized?.()
    throw new Error('Unauthorized')
  }
  if (!res.ok) throw new Error(`Failed to put ${url}: ${res.status}`)
  return res.json()
}

async function deleteRequest(url: string): Promise<void> {
  const res = await fetch(url, { method: 'DELETE' })
  if (res.status === 401) {
    onUnauthorized?.()
    throw new Error('Unauthorized')
  }
  if (!res.ok) throw new Error(`Failed to delete ${url}: ${res.status}`)
}

// Existing
export async function fetchOverview(): Promise<Overview> {
  return fetchJson(`${API_BASE}/stats/overview`);
}

export async function getStats(): Promise<Stats> {
  return fetchJson(`${API_BASE}/stats`);
}

export async function fetchAttempts(params?: {
  status?: string;
  source?: string;
  page?: number;
  per_page?: number;
}): Promise<AttemptsResponse> {
  const searchParams = new URLSearchParams();
  if (params?.status) searchParams.set('status', params.status);
  if (params?.source) searchParams.set('source', params.source);
  if (params?.page) searchParams.set('page', params.page.toString());
  if (params?.per_page) searchParams.set('per_page', params.per_page.toString());
  return fetchJson(`${API_BASE}/attempts?${searchParams}`);
}

export async function getHealth(): Promise<Health> {
  return fetchJson(`${API_BASE}/health`);
}

export async function getRetries(): Promise<RetriesResponse> {
  return fetchJson(`${API_BASE}/retries`);
}

export async function getSources(): Promise<SourcesResponse> {
  return fetchJson(`${API_BASE}/sources`);
}

// New fetchers

export async function fetchActivity(params?: {
  limit?: number;
  source?: string;
}): Promise<ActivityLogEntry[]> {
  const searchParams = new URLSearchParams();
  if (params?.limit) searchParams.set('limit', params.limit.toString());
  if (params?.source) searchParams.set('source', params.source);
  return fetchJson(`${API_BASE}/activity?${searchParams}`);
}

export async function fetchAttemptDetail(attemptId: number): Promise<AttemptDetailResponse> {
  return fetchJson(`${API_BASE}/attempts/${attemptId}/detail`);
}

export async function fetchAttemptExecutionLog(
  attemptId: number,
  executionId: number,
  stream: 'stdout' | 'stderr' | 'events',
): Promise<AttemptExecutionLogResponse> {
  return fetchJson(
    `${API_BASE}/attempts/${attemptId}/logs/${executionId}/${stream}`,
  );
}

export async function fetchAnalyticsSummary(): Promise<AnalyticsSummary> {
  return fetchJson(`${API_BASE}/analytics/summary`);
}

export async function fetchMetrics(params?: {
  name?: string;
  period?: string;
  limit?: number;
}): Promise<ProcessingMetric[]> {
  const searchParams = new URLSearchParams();
  if (params?.name) searchParams.set('name', params.name);
  if (params?.period) searchParams.set('period', params.period);
  if (params?.limit) searchParams.set('limit', params.limit.toString());
  return fetchJson(`${API_BASE}/metrics?${searchParams}`);
}

export async function fetchErrors(limit = 50): Promise<ErrorPattern[]> {
  return fetchJson(`${API_BASE}/errors?limit=${limit}`);
}

export async function fetchPrs(params?: {
  status?: string;
  limit?: number;
}): Promise<PrRecord[]> {
  const searchParams = new URLSearchParams();
  if (params?.status) searchParams.set('status', params.status);
  if (params?.limit) searchParams.set('limit', params.limit.toString());
  return fetchJson(`${API_BASE}/prs?${searchParams}`);
}

export async function fetchPrAnalytics(): Promise<PrAnalytics> {
  return fetchJson(`${API_BASE}/prs/analytics`);
}

export async function fetchFeedback(params?: {
  source?: string;
  limit?: number;
}): Promise<FixOutcome[]> {
  const searchParams = new URLSearchParams();
  if (params?.source) searchParams.set('source', params.source);
  if (params?.limit) searchParams.set('limit', params.limit.toString());
  return fetchJson(`${API_BASE}/feedback?${searchParams}`);
}

export async function fetchRegressions(status?: string): Promise<RegressionWatch[]> {
  const searchParams = new URLSearchParams();
  if (status) searchParams.set('status', status);
  return fetchJson(`${API_BASE}/regressions?${searchParams}`);
}

export async function fetchRegressionChecks(watchId: number): Promise<RegressionCheck[]> {
  return fetchJson(`${API_BASE}/regressions/${watchId}/checks`);
}

export async function fetchExperiments(): Promise<PromptExperiment[]> {
  return fetchJson(`${API_BASE}/experiments`);
}

export async function fetchRepos(): Promise<StoredIndexedRepo[]> {
  return fetchJson(`${API_BASE}/repos`);
}

export async function fetchRepoStats(): Promise<IndexStats> {
  return fetchJson(`${API_BASE}/repos/stats`);
}

export async function fetchDependencies(): Promise<StoredDependency[]> {
  return fetchJson(`${API_BASE}/repos/dependencies`);
}

export async function fetchInferenceStats(): Promise<InferenceStats> {
  return fetchJson(`${API_BASE}/inference/stats`);
}

export async function fetchInferenceHistory(limit = 50): Promise<InferenceHistoryEntry[]> {
  return fetchJson(`${API_BASE}/inference/history?limit=${limit}`);
}

export async function fetchTelemetryOverview(): Promise<TelemetryOverview> {
  return fetchJson(`${API_BASE}/telemetry/overview`);
}

export async function fetchTelemetryTimeseries(params?: {
  period?: string;
  bucket_minutes?: number;
}): Promise<TelemetryTimeseries> {
  const searchParams = new URLSearchParams();
  if (params?.period) searchParams.set('period', params.period);
  if (params?.bucket_minutes) {
    searchParams.set('bucket_minutes', params.bucket_minutes.toString());
  }
  return fetchJson(`${API_BASE}/telemetry/timeseries?${searchParams}`);
}

export async function fetchTelemetryPipeline(params?: {
  period?: string;
}): Promise<TelemetryPipeline> {
  const searchParams = new URLSearchParams();
  if (params?.period) searchParams.set('period', params.period);
  return fetchJson(`${API_BASE}/telemetry/pipeline?${searchParams}`);
}

export async function fetchTelemetryLatency(params?: {
  period?: string;
}): Promise<TelemetryLatency> {
  const searchParams = new URLSearchParams();
  if (params?.period) searchParams.set('period', params.period);
  return fetchJson(`${API_BASE}/telemetry/latency?${searchParams}`);
}

// ─── Config API ──────────────────────────────────

export interface ConfigResponse {
  content: string
  path: string
}

export async function fetchConfig(): Promise<ConfigResponse> {
  return fetchJson(`${API_BASE}/config`)
}

export async function saveConfig(content: string): Promise<{ ok: boolean; message: string }> {
  return putJson(`${API_BASE}/config`, { content })
}

// ─── Auth API ────────────────────────────────────

export async function login(email: string, password: string): Promise<LoginResponse> {
  return postJson(`${API_BASE}/auth/login`, { email, password })
}

export async function logout(): Promise<void> {
  await postJson(`${API_BASE}/auth/logout`)
}

export async function getMe(): Promise<AuthUser> {
  return fetchJson(`${API_BASE}/auth/me`)
}

// ─── User Management API ─────────────────────────

export async function fetchUsers(): Promise<UserRecord[]> {
  return fetchJson(`${API_BASE}/users`)
}

export async function getUser(id: number): Promise<UserRecord> {
  return fetchJson(`${API_BASE}/users/${id}`)
}

export async function createUser(data: {
  email: string; password: string; name: string; role: string
}): Promise<UserRecord> {
  return postJson(`${API_BASE}/users`, data)
}

export async function updateUser(id: number, data: {
  email?: string; password?: string; name?: string; role?: string
}): Promise<UserRecord> {
  return putJson(`${API_BASE}/users/${id}`, data)
}

export async function deleteUser(id: number): Promise<void> {
  return deleteRequest(`${API_BASE}/users/${id}`)
}

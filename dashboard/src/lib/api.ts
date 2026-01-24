const API_BASE = '/api';

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

export async function fetchOverview(): Promise<Overview> {
  const res = await fetch(`${API_BASE}/stats/overview`);
  if (!res.ok) throw new Error('Failed to fetch overview');
  return res.json();
}

export async function getStats(): Promise<Stats> {
  const res = await fetch(`${API_BASE}/stats`);
  if (!res.ok) throw new Error('Failed to fetch stats');
  return res.json();
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

  const res = await fetch(`${API_BASE}/attempts?${searchParams}`);
  if (!res.ok) throw new Error('Failed to fetch attempts');
  return res.json();
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

export async function getHealth(): Promise<Health> {
  const res = await fetch(`${API_BASE}/health`);
  if (!res.ok) throw new Error('Failed to fetch health');
  return res.json();
}

export interface RetriesResponse {
  retryable: AttemptSummary[];
  ready: AttemptSummary[];
  max_retries: number;
}

export async function getRetries(): Promise<RetriesResponse> {
  const res = await fetch(`${API_BASE}/retries`);
  if (!res.ok) throw new Error('Failed to fetch retries');
  return res.json();
}

export interface SourceInfo {
  name: string;
  enabled: boolean;
  config: Record<string, unknown>;
}

export interface SourcesResponse {
  sources: SourceInfo[];
}

export async function getSources(): Promise<SourcesResponse> {
  const res = await fetch(`${API_BASE}/sources`);
  if (!res.ok) throw new Error('Failed to fetch sources');
  return res.json();
}

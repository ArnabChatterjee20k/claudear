import { useState } from 'react'
import useSWR from 'swr'
import {
  fetchTelemetryOverview,
  fetchTelemetryPipeline,
  fetchTelemetryLatency,
  fetchTelemetryTimeseries,
  type TelemetryOverview,
  type TelemetryPipeline,
  type TelemetryLatency,
  type TelemetryTimeseries,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { Select } from '../components/ui/select'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import {
  Activity,
  Clock3,
  GitPullRequest,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  Timer,
  TrendingUp,
} from 'lucide-react'
import {
  AreaChart,
  Area,
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
  Legend,
} from 'recharts'

const periodOptions = [
  { value: 'hour', label: 'Last Hour' },
  { value: 'day', label: 'Last Day' },
  { value: 'week', label: 'Last Week' },
  { value: 'month', label: 'Last Month' },
]

const bucketByPeriod: Record<string, number> = {
  hour: 5,
  day: 15,
  week: 60,
  month: 360,
}

function formatUptime(seconds: number): string {
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const mins = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${mins}m`
  return `${mins}m`
}

function formatSecs(value: number | null | undefined): string {
  if (value == null) return '--'
  return `${value.toFixed(1)}s`
}

function formatRate(value: number | null | undefined): string {
  if (value == null) return '--'
  return `${(value * 100).toFixed(1)}%`
}

function formatCount(value: number | null | undefined): string {
  if (value == null) return '--'
  return Math.round(value).toLocaleString()
}

export default function TelemetryPage() {
  const [period, setPeriod] = useState('week')
  const bucketMinutes = bucketByPeriod[period] || 60

  const { data: overview, error: overviewError, isLoading: overviewLoading } = useSWR<TelemetryOverview>(
    'telemetry-overview',
    fetchTelemetryOverview,
    { refreshInterval: 10000 },
  )

  const { data: series, isLoading: seriesLoading } = useSWR<TelemetryTimeseries>(
    ['telemetry-timeseries', period],
    () => fetchTelemetryTimeseries({ period, bucket_minutes: bucketMinutes }),
    { refreshInterval: 15000 },
  )

  const { data: pipeline, isLoading: pipelineLoading } = useSWR<TelemetryPipeline>(
    ['telemetry-pipeline', period],
    () => fetchTelemetryPipeline({ period }),
    { refreshInterval: 15000 },
  )

  const { data: latency, isLoading: latencyLoading } = useSWR<TelemetryLatency>(
    ['telemetry-latency', period],
    () => fetchTelemetryLatency({ period }),
    { refreshInterval: 15000 },
  )

  const chartData = (series?.points || []).map(p => ({
    time: new Date(p.bucket_start).toLocaleString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    }),
    total: p.total,
    success: p.success + p.merged,
    failed: p.failed + p.closed + p.cannot_fix,
    pending: p.pending,
  }))

  const latencyHistogramData = (latency?.histogram || []).map(bucket => ({
    bucket: bucket.label,
    count: bucket.count,
  }))

  return (
    <div className="space-y-6">
      <PageHeader
        title="Telemetry"
        description="End-to-end runtime telemetry, queue health, throughput, and diagnostics"
      />

      <div className="flex items-center gap-4">
        <Select
          value={period}
          onChange={setPeriod}
          options={periodOptions}
          placeholder="Select period"
          className="w-48"
        />
      </div>

      {overviewError && (
        <div className="text-destructive text-sm">Failed to load telemetry overview.</div>
      )}

      {overviewLoading && (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={i} className="h-28" />
          ))}
        </div>
      )}

      {overview && (
        <>
          <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
            <StatsCard
              title="Uptime"
              value={formatUptime(overview.uptime_secs)}
              icon={<Clock3 className="h-4 w-4 text-blue-500" />}
              description="API process uptime"
            />
            <StatsCard
              title="Pending Attempts"
              value={overview.queue.pending_attempts}
              icon={<Activity className="h-4 w-4 text-yellow-500" />}
              description="Current pending work"
            />
            <StatsCard
              title="Ready Retries"
              value={overview.queue.ready_retries}
              icon={<RefreshCw className="h-4 w-4 text-orange-500" />}
              description="Eligible to re-run now"
            />
            <StatsCard
              title="Open PRs"
              value={overview.queue.open_prs}
              icon={<GitPullRequest className="h-4 w-4 text-green-500" />}
              description="PR queue size"
            />
            <StatsCard
              title="Monitoring Watches"
              value={overview.queue.watches_monitoring}
              icon={<ShieldCheck className="h-4 w-4 text-indigo-500" />}
              description="Regression checks active"
            />
            <StatsCard
              title="Regressed Watches"
              value={overview.queue.watches_regressed}
              icon={<ShieldAlert className="h-4 w-4 text-red-500" />}
              description="Fixes that regressed"
            />
            <StatsCard
              title="Avg Proc Time (24h)"
              value={formatSecs(overview.processing_time.last_24h.avg_secs)}
              icon={<Timer className="h-4 w-4 text-cyan-500" />}
              description={`p95 ${formatSecs(overview.processing_time.last_24h.p95_secs)}`}
            />
            <StatsCard
              title="PR Merge Rate"
              value={
                overview.pr_analytics.merge_rate != null
                  ? `${(overview.pr_analytics.merge_rate * 100).toFixed(1)}%`
                  : '--'
              }
              icon={<TrendingUp className="h-4 w-4 text-emerald-500" />}
              description={`${overview.pr_analytics.merged} merged`}
            />
            <StatsCard
              title="Poll Cycles"
              value={formatCount(pipeline?.poll_load.poll_cycles)}
              icon={<Activity className="h-4 w-4 text-violet-500" />}
              description={`Period: ${period}`}
            />
            <StatsCard
              title="Avg Poll Cycle"
              value={formatSecs(pipeline?.poll_load.avg_cycle_secs)}
              icon={<Timer className="h-4 w-4 text-sky-500" />}
              description={`p95 ${formatSecs(pipeline?.poll_load.p95_cycle_secs)}`}
            />
            <StatsCard
              title="Match Rate"
              value={formatRate(pipeline?.conversion.match_rate)}
              icon={<TrendingUp className="h-4 w-4 text-lime-500" />}
              description={`${formatCount(pipeline?.totals.matched)} matched`}
            />
            <StatsCard
              title="PR Yield"
              value={formatRate(pipeline?.conversion.pr_yield_rate)}
              icon={<GitPullRequest className="h-4 w-4 text-green-500" />}
              description={`${formatCount(pipeline?.totals.pr_created)} PRs`}
            />
          </div>

          <Card>
            <CardHeader>
              <CardTitle>Window Performance</CardTitle>
              <CardDescription>Throughput and reliability by rolling time window</CardDescription>
            </CardHeader>
            <CardContent>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b">
                      <th className="text-left py-2 font-medium">Window</th>
                      <th className="text-right py-2 font-medium">Processed</th>
                      <th className="text-right py-2 font-medium">Success Rate</th>
                      <th className="text-right py-2 font-medium">Error Rate</th>
                      <th className="text-right py-2 font-medium">Throughput/hr</th>
                    </tr>
                  </thead>
                  <tbody>
                    {overview.windows.map(w => (
                      <tr key={w.window} className="border-b last:border-0">
                        <td className="py-2 font-medium">{w.window}</td>
                        <td className="text-right py-2">{w.processed}</td>
                        <td className="text-right py-2 text-green-600">{w.success_rate.toFixed(1)}%</td>
                        <td className="text-right py-2 text-red-600">{w.error_rate.toFixed(1)}%</td>
                        <td className="text-right py-2">{w.throughput_per_hour.toFixed(2)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Attempt Timeseries</CardTitle>
              <CardDescription>Success, failure, and pending volume over time</CardDescription>
            </CardHeader>
            <CardContent>
              {seriesLoading && <Skeleton className="h-72 w-full" />}
              {!seriesLoading && chartData.length === 0 && (
                <p className="text-sm text-muted-foreground py-8 text-center">
                  No telemetry samples yet
                </p>
              )}
              {!seriesLoading && chartData.length > 0 && (
                <div className="h-72">
                  <ResponsiveContainer width="100%" height="100%">
                    <AreaChart data={chartData}>
                      <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                      <XAxis dataKey="time" tick={{ fontSize: 12 }} className="text-muted-foreground" />
                      <YAxis tick={{ fontSize: 12 }} className="text-muted-foreground" />
                      <Tooltip />
                      <Legend />
                      <Area type="monotone" dataKey="success" stackId="1" stroke="#22c55e" fill="#22c55e55" />
                      <Area type="monotone" dataKey="failed" stackId="1" stroke="#ef4444" fill="#ef444455" />
                      <Area type="monotone" dataKey="pending" stackId="1" stroke="#f59e0b" fill="#f59e0b55" />
                    </AreaChart>
                  </ResponsiveContainer>
                </div>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Poll Pipeline</CardTitle>
              <CardDescription>Stage conversion, retries, PR checks, and cascade outcomes</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {pipelineLoading && <Skeleton className="h-40 w-full" />}
              {!pipelineLoading && pipeline && (
                <>
                  <div className="grid gap-3 md:grid-cols-4">
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Fetched</div>
                      <div className="text-xl font-semibold">{formatCount(pipeline.totals.fetched)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Queued</div>
                      <div className="text-xl font-semibold">{formatCount(pipeline.totals.queued)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Processed</div>
                      <div className="text-xl font-semibold">{formatCount(pipeline.totals.processed)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Retry Executed</div>
                      <div className="text-xl font-semibold">{formatCount(pipeline.totals.retries_executed)}</div>
                    </div>
                  </div>
                  <div className="grid gap-3 md:grid-cols-4 text-sm">
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">Match</div>
                      <div className="font-medium">{formatRate(pipeline.conversion.match_rate)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">Queue</div>
                      <div className="font-medium">{formatRate(pipeline.conversion.queue_rate)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">Processing</div>
                      <div className="font-medium">{formatRate(pipeline.conversion.processing_rate)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">PR Yield</div>
                      <div className="font-medium">{formatRate(pipeline.conversion.pr_yield_rate)}</div>
                    </div>
                  </div>
                  <div className="grid gap-3 md:grid-cols-3 text-sm">
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">PR Status Checks</div>
                      <div className="font-medium">{formatCount(pipeline.totals.pr_status_checks)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">Merged/Closed</div>
                      <div className="font-medium">
                        {formatCount(pipeline.totals.pr_status_merged)} / {formatCount(pipeline.totals.pr_status_closed)}
                      </div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-muted-foreground">Cascade Triggered</div>
                      <div className="font-medium">
                        {formatCount(pipeline.totals.cascade_triggered)} ({formatCount(pipeline.totals.cascade_failed)} failed)
                      </div>
                    </div>
                  </div>
                  <div className="overflow-x-auto">
                    <table className="w-full text-sm">
                      <thead>
                        <tr className="border-b">
                          <th className="text-left py-2 font-medium">Source</th>
                          <th className="text-right py-2 font-medium">Fetched</th>
                          <th className="text-right py-2 font-medium">Queued</th>
                          <th className="text-right py-2 font-medium">Processed</th>
                          <th className="text-right py-2 font-medium">PRs</th>
                          <th className="text-right py-2 font-medium">PR Yield</th>
                        </tr>
                      </thead>
                      <tbody>
                        {pipeline.per_source.map(row => (
                          <tr key={row.source} className="border-b last:border-0">
                            <td className="py-2 font-medium capitalize">{row.source}</td>
                            <td className="text-right py-2">{formatCount(row.fetched)}</td>
                            <td className="text-right py-2">{formatCount(row.queued)}</td>
                            <td className="text-right py-2">{formatCount(row.processed)}</td>
                            <td className="text-right py-2">{formatCount(row.pr_created)}</td>
                            <td className="text-right py-2">{formatRate(row.pr_yield_rate)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Processing Latency</CardTitle>
              <CardDescription>Latency histogram and per-status processing summaries</CardDescription>
            </CardHeader>
            <CardContent className="space-y-4">
              {latencyLoading && <Skeleton className="h-56 w-full" />}
              {!latencyLoading && latency && (
                <>
                  <div className="grid gap-3 md:grid-cols-4">
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Samples</div>
                      <div className="text-xl font-semibold">{latency.overall.samples}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">Avg</div>
                      <div className="text-xl font-semibold">{formatSecs(latency.overall.avg_secs)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">p95</div>
                      <div className="text-xl font-semibold">{formatSecs(latency.overall.p95_secs)}</div>
                    </div>
                    <div className="rounded border p-3">
                      <div className="text-xs text-muted-foreground">p99</div>
                      <div className="text-xl font-semibold">{formatSecs(latency.overall.p99_secs)}</div>
                    </div>
                  </div>
                  <div className="h-52">
                    <ResponsiveContainer width="100%" height="100%">
                      <BarChart data={latencyHistogramData}>
                        <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                        <XAxis dataKey="bucket" tick={{ fontSize: 12 }} className="text-muted-foreground" />
                        <YAxis tick={{ fontSize: 12 }} className="text-muted-foreground" />
                        <Tooltip />
                        <Bar dataKey="count" fill="#0ea5e9" radius={[4, 4, 0, 0]} />
                      </BarChart>
                    </ResponsiveContainer>
                  </div>
                  <div className="overflow-x-auto">
                    <table className="w-full text-sm">
                      <thead>
                        <tr className="border-b">
                          <th className="text-left py-2 font-medium">Status</th>
                          <th className="text-right py-2 font-medium">Samples</th>
                          <th className="text-right py-2 font-medium">Avg</th>
                          <th className="text-right py-2 font-medium">p95</th>
                          <th className="text-right py-2 font-medium">Max</th>
                        </tr>
                      </thead>
                      <tbody>
                        {latency.by_status.map(row => (
                          <tr key={row.status} className="border-b last:border-0">
                            <td className="py-2 font-medium">{row.status}</td>
                            <td className="text-right py-2">{row.summary.samples}</td>
                            <td className="text-right py-2">{formatSecs(row.summary.avg_secs)}</td>
                            <td className="text-right py-2">{formatSecs(row.summary.p95_secs)}</td>
                            <td className="text-right py-2">{formatSecs(row.summary.max_secs)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </>
              )}
            </CardContent>
          </Card>

          <div className="grid gap-6 md:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>Source Breakdown</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b">
                        <th className="text-left py-2 font-medium">Source</th>
                        <th className="text-right py-2 font-medium">Total</th>
                        <th className="text-right py-2 font-medium">Pending</th>
                        <th className="text-right py-2 font-medium">Retryable</th>
                        <th className="text-right py-2 font-medium">Success Rate</th>
                      </tr>
                    </thead>
                    <tbody>
                      {overview.source_breakdown.map(s => (
                        <tr key={s.source} className="border-b last:border-0">
                          <td className="py-2 font-medium capitalize">{s.source}</td>
                          <td className="text-right py-2">{s.total}</td>
                          <td className="text-right py-2">{s.pending}</td>
                          <td className="text-right py-2">{s.retryable}</td>
                          <td className="text-right py-2 text-green-600">{s.success_rate.toFixed(1)}%</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Top Errors</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="space-y-2">
                  {overview.top_errors.length === 0 && (
                    <p className="text-sm text-muted-foreground">No recurring errors recorded.</p>
                  )}
                  {overview.top_errors.map(err => (
                    <div key={err.id} className="border rounded p-3">
                      <div className="text-sm font-medium">{err.error_type || 'unknown'}</div>
                      <div className="text-xs text-muted-foreground">
                        count {err.occurrence_count} · last {new Date(err.last_seen).toLocaleString()}
                      </div>
                      {err.error_message && (
                        <div className="text-sm mt-1 line-clamp-2">{err.error_message}</div>
                      )}
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          </div>

          <div className="grid gap-6 md:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>Activity (Last Hour)</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="space-y-2 text-sm">
                  {Object.keys(overview.activity_last_hour).length === 0 && (
                    <p className="text-muted-foreground">No activity recorded.</p>
                  )}
                  {Object.entries(overview.activity_last_hour)
                    .sort(([, a], [, b]) => b - a)
                    .map(([name, count]) => (
                      <div key={name} className="flex items-center justify-between border-b pb-1 last:border-0">
                        <span>{name}</span>
                        <span className="font-medium">{count}</span>
                      </div>
                    ))}
                </div>
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle>Metric Samples (24h)</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="space-y-2 text-sm">
                  {Object.entries(overview.metric_counts_last_24h)
                    .sort(([, a], [, b]) => b - a)
                    .map(([name, count]) => (
                      <div key={name} className="flex items-center justify-between border-b pb-1 last:border-0">
                        <span>{name}</span>
                        <span className="font-medium">{count}</span>
                      </div>
                    ))}
                </div>
              </CardContent>
            </Card>
          </div>

          {overview.diagnostics && (
            <Card>
              <CardHeader>
                <CardTitle>Diagnostics</CardTitle>
                <CardDescription>Raw table-level counts from local tracker storage</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="grid gap-2 md:grid-cols-3 text-sm">
                  {Object.entries(overview.diagnostics).map(([key, value]) => (
                    <div key={key} className="border rounded p-2">
                      <div className="text-muted-foreground">{key}</div>
                      <div className="font-medium break-all">{String(value)}</div>
                    </div>
                  ))}
                </div>
              </CardContent>
            </Card>
          )}
        </>
      )}
    </div>
  )
}

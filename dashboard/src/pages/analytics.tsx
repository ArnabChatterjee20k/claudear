import useSWR from 'swr'
import {
  fetchAnalyticsSummary,
  fetchMetrics,
  type AnalyticsSummary,
  type ProcessingMetric,
} from '../lib/api'
import { parseUTCDate, formatMins } from '../lib/formatters'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { BarChart3, Clock, CheckCircle2, AlertTriangle, DollarSign, TrendingUp } from 'lucide-react'
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'

export default function AnalyticsPage() {
  const { data: summary, isLoading: summaryLoading } = useSWR<AnalyticsSummary>(
    'analytics-summary',
    fetchAnalyticsSummary,
    { refreshInterval: 30000 },
  )

  const { data: metrics, isLoading: metricsLoading } = useSWR<ProcessingMetric[]>(
    'processing-time-metrics',
    () => fetchMetrics({ name: 'processing_time', period: 'week', limit: 100 }),
    { refreshInterval: 30000 },
  )

  const chartData = (metrics || []).map(m => ({
    time: parseUTCDate(m.timestamp).toLocaleString(undefined, {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit',
    }),
    value: m.metric_value,
  }))

  return (
    <div className="space-y-6">
      <PageHeader title="Analytics" description="Trends, metrics, and performance insights" />

      {summaryLoading && (
        <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-28" />
          ))}
        </div>
      )}

      {summary && (
        <>
          <div className="grid gap-4 md:grid-cols-3 xl:grid-cols-6">
            <StatsCard
              title="Success Rate"
              value={`${summary.success_rate.toFixed(1)}%`}
              icon={<CheckCircle2 className="h-4 w-4 text-green-500" />}
              description={`${summary.total_successful} of ${summary.total_processed} successful`}
            />
            <StatsCard
              title="Total Processed"
              value={summary.total_processed}
              icon={<BarChart3 className="h-4 w-4 text-blue-500" />}
              description={`${summary.total_merged} merged`}
            />
            <StatsCard
              title="Avg Processing Time"
              value={
                summary.avg_processing_time_secs != null
                  ? `${summary.avg_processing_time_secs.toFixed(1)}s`
                  : '--'
              }
              icon={<Clock className="h-4 w-4 text-yellow-500" />}
              description="Mean time per attempt"
            />
            <StatsCard
              title="Most Common Error"
              value={summary.most_common_error || 'None'}
              icon={<AlertTriangle className="h-4 w-4 text-red-500" />}
              description="Top recurring error"
            />
            <StatsCard
              title="Avg Time to PR"
              value={formatMins(summary.avg_time_to_pr_mins ?? null)}
              icon={<Clock className="h-4 w-4 text-indigo-500" />}
              description="Issue → PR creation"
            />
            <StatsCard
              title="Est. Cost / Fix"
              value={
                summary.cost_estimate
                  ? `$${summary.cost_estimate.avg_cost_per_fix.toFixed(3)}`
                  : '--'
              }
              icon={<DollarSign className="h-4 w-4 text-emerald-500" />}
              description={
                summary.cost_estimate
                  ? `$${summary.cost_estimate.total_cost.toFixed(2)} total (${summary.cost_estimate.period})`
                  : 'No cost data'
              }
            />
          </div>

          {Object.keys(summary.success_rate_by_source).length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Success Rate by Source</CardTitle>
                <CardDescription>Percentage of successful attempts per source</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="space-y-3">
                  {Object.entries(summary.success_rate_by_source)
                    .sort(([, a], [, b]) => b - a)
                    .map(([source, rate]) => (
                      <div key={source} className="flex items-center gap-3">
                        <span className="w-24 text-sm font-medium capitalize">{source}</span>
                        <div className="flex-1 h-6 bg-muted rounded overflow-hidden">
                          <div
                            className="h-full bg-green-500 rounded transition-all"
                            style={{ width: `${Math.min(rate, 100)}%` }}
                          />
                        </div>
                        <span className="w-16 text-sm text-right font-medium">
                          {rate.toFixed(1)}%
                        </span>
                      </div>
                    ))}
                </div>
              </CardContent>
            </Card>
          )}

          {(summary.mttr_trend ?? []).length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle className="flex items-center gap-2">
                  <TrendingUp className="h-5 w-5" />
                  MTTR Trend
                </CardTitle>
                <CardDescription>Weekly mean time to resolve (minutes)</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="h-64">
                  <ResponsiveContainer width="100%" height="100%">
                    <LineChart data={summary.mttr_trend}>
                      <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                      <XAxis dataKey="period_start" tick={{ fontSize: 12 }} className="text-muted-foreground" />
                      <YAxis tick={{ fontSize: 12 }} className="text-muted-foreground" label={{ value: 'minutes', angle: -90, position: 'insideLeft', style: { fontSize: 12 } }} />
                      <Tooltip />
                      <Line type="monotone" dataKey="mttr_minutes" stroke="#8b5cf6" strokeWidth={2} dot={{ r: 3 }} name="MTTR (min)" />
                    </LineChart>
                  </ResponsiveContainer>
                </div>
              </CardContent>
            </Card>
          )}

          {(summary.repo_leaderboard ?? []).length > 0 && (
            <Card>
              <CardHeader>
                <CardTitle>Repo Leaderboard</CardTitle>
                <CardDescription>Per-repository fix performance</CardDescription>
              </CardHeader>
              <CardContent>
                <div className="overflow-x-auto">
                  <table className="w-full text-sm">
                    <thead>
                      <tr className="border-b">
                        <th className="text-left py-2 font-medium">#</th>
                        <th className="text-left py-2 font-medium">Repository</th>
                        <th className="text-right py-2 font-medium">Total</th>
                        <th className="text-right py-2 font-medium">Success Rate</th>
                        <th className="text-right py-2 font-medium">Merge Rate</th>
                        <th className="text-right py-2 font-medium">Avg Time to Merge</th>
                      </tr>
                    </thead>
                    <tbody>
                      {summary.repo_leaderboard!.map((entry, i) => (
                        <tr key={entry.repo} className="border-b last:border-0">
                          <td className="py-2">{i + 1}</td>
                          <td className="py-2 font-medium">{entry.repo}</td>
                          <td className="text-right py-2">{entry.total}</td>
                          <td className="text-right py-2 text-green-600">{(entry.success_rate * 100).toFixed(1)}%</td>
                          <td className="text-right py-2 text-purple-600">{(entry.merge_rate * 100).toFixed(1)}%</td>
                          <td className="text-right py-2">{formatMins(entry.avg_time_to_merge_mins ?? null)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </CardContent>
            </Card>
          )}
        </>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <TrendingUp className="h-5 w-5" />
            Processing Time
          </CardTitle>
          <CardDescription>Time series of processing durations</CardDescription>
        </CardHeader>
        <CardContent>
          {metricsLoading && <Skeleton className="h-64 w-full" />}
          {!metricsLoading && chartData.length === 0 && (
            <p className="text-sm text-muted-foreground py-8 text-center">
              No metrics data available yet
            </p>
          )}
          {!metricsLoading && chartData.length > 0 && (
            <div className="h-64">
              <ResponsiveContainer width="100%" height="100%">
                <LineChart data={chartData}>
                  <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                  <XAxis
                    dataKey="time"
                    tick={{ fontSize: 12 }}
                    className="text-muted-foreground"
                  />
                  <YAxis
                    tick={{ fontSize: 12 }}
                    className="text-muted-foreground"
                    label={{
                      value: 'seconds',
                      angle: -90,
                      position: 'insideLeft',
                      style: { fontSize: 12 },
                    }}
                  />
                  <Tooltip />
                  <Line
                    type="monotone"
                    dataKey="value"
                    stroke="hsl(var(--primary))"
                    strokeWidth={2}
                    dot={false}
                  />
                </LineChart>
              </ResponsiveContainer>
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  )
}

import useSWR from 'swr'
import {
  fetchAnalyticsSummary,
  fetchMetrics,
  type AnalyticsSummary,
  type ProcessingMetric,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { BarChart3, Clock, CheckCircle2, AlertTriangle, TrendingUp } from 'lucide-react'
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
    time: new Date(m.timestamp).toLocaleString(undefined, {
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
          <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
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

import useSWR from 'swr'
import { fetchOverview, getRetries, type Overview, type AttemptSummary, type RetriesResponse } from '../lib/api'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '../components/ui/card'
import { StatsCard } from '../components/shared/stats-card'
import { StatusBadge } from '../components/shared/status-badge'
import { getTimeAgo } from '../components/shared/time-ago'
import { PageHeader } from '../components/layout/page-header'
import {
  Activity,
  CheckCircle2,
  XCircle,
  GitMerge,
  Clock,
  AlertTriangle,
  ExternalLink,
  RefreshCw,
} from 'lucide-react'

export default function OverviewPage() {
  const { data: overview, error: overviewError, isLoading: overviewLoading } = useSWR<Overview>('overview', fetchOverview, {
    refreshInterval: 10000,
  })

  const { data: retries } = useSWR<RetriesResponse>('retries', getRetries, {
    refreshInterval: 10000,
  })

  if (overviewError) {
    return (
      <div className="flex items-center justify-center h-full">
        <Card className="w-[400px]">
          <CardHeader>
            <CardTitle className="text-destructive">Error</CardTitle>
            <CardDescription>
              Failed to connect to the API. Make sure the dashboard server is running.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <code className="text-sm bg-muted p-2 rounded block">claudear dashboard</code>
          </CardContent>
        </Card>
      </div>
    )
  }

  if (overviewLoading || !overview) {
    return (
      <div className="flex items-center justify-center h-full">
        <div className="animate-pulse text-muted-foreground">Loading...</div>
      </div>
    )
  }

  const { stats, success_rate, merge_rate, recent_attempts, sources } = overview

  return (
    <div className="space-y-6">
      <PageHeader title="Overview" description="Monitor automated issue fixing" />

      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-4">
        <StatsCard title="Total Attempts" value={stats.total} icon={<Activity className="h-4 w-4" />} description="All fix attempts" />
        <StatsCard title="Success Rate" value={`${success_rate.toFixed(1)}%`} icon={<CheckCircle2 className="h-4 w-4 text-green-500" />} description={`${stats.success + stats.merged} successful`} />
        <StatsCard title="Merge Rate" value={`${merge_rate.toFixed(1)}%`} icon={<GitMerge className="h-4 w-4 text-purple-500" />} description={`${stats.merged} merged PRs`} />
        <StatsCard title="Pending" value={stats.pending} icon={<Clock className="h-4 w-4 text-yellow-500" />} description="In progress" />
      </div>

      <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-6">
        <StatusCard status="pending" count={stats.pending} />
        <StatusCard status="success" count={stats.success} />
        <StatusCard status="merged" count={stats.merged} />
        <StatusCard status="closed" count={stats.closed} />
        <StatusCard status="failed" count={stats.failed} />
        <StatusCard status="cannot_fix" count={stats.cannot_fix} />
      </div>

      {retries && (retries.retryable.length > 0 || retries.ready.length > 0) && (
        <Card>
          <CardHeader>
            <CardTitle className="flex items-center gap-2">
              <RefreshCw className="h-5 w-5" />
              Retries
            </CardTitle>
            <CardDescription>
              {retries.ready.length} ready now · {retries.retryable.length} total retryable · Max {retries.max_retries} retries
            </CardDescription>
          </CardHeader>
          <CardContent>
            {retries.ready.length > 0 && (
              <div className="mb-4">
                <h4 className="text-sm font-medium mb-2 text-green-600">Ready for Retry</h4>
                <div className="space-y-2">
                  {retries.ready.map(attempt => (
                    <AttemptRow key={attempt.id} attempt={attempt} />
                  ))}
                </div>
              </div>
            )}
            {retries.retryable.length > 0 && retries.ready.length < retries.retryable.length && (
              <div>
                <h4 className="text-sm font-medium mb-2 text-muted-foreground">Waiting for Backoff</h4>
                <div className="space-y-2">
                  {retries.retryable
                    .filter(a => !retries.ready.some(r => r.id === a.id))
                    .map(attempt => (
                      <AttemptRow key={attempt.id} attempt={attempt} />
                    ))}
                </div>
              </div>
            )}
          </CardContent>
        </Card>
      )}

      <div className="grid gap-6 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>Sources</CardTitle>
            <CardDescription>Issues by source</CardDescription>
          </CardHeader>
          <CardContent>
            {sources.length === 0 ? (
              <p className="text-muted-foreground">No sources configured</p>
            ) : (
              <div className="space-y-4">
                {sources.map(source => (
                  <div key={source.name} className="flex items-center justify-between">
                    <div>
                      <p className="font-medium capitalize">{source.name}</p>
                      <p className="text-sm text-muted-foreground">
                        {source.total} total · {source.merged} merged
                      </p>
                    </div>
                    <div className="text-right">
                      <p className="font-medium">{source.success_rate.toFixed(1)}%</p>
                      <p className="text-sm text-muted-foreground">success</p>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Recent Attempts</CardTitle>
            <CardDescription>Latest fix attempts</CardDescription>
          </CardHeader>
          <CardContent>
            {recent_attempts.length === 0 ? (
              <p className="text-muted-foreground">No attempts yet</p>
            ) : (
              <div className="space-y-3">
                {recent_attempts.slice(0, 8).map(attempt => (
                  <AttemptRow key={attempt.id} attempt={attempt} />
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      {Object.keys(stats.by_source).length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>Source Breakdown</CardTitle>
            <CardDescription>Detailed statistics by source</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="overflow-x-auto">
              <table className="w-full text-sm">
                <thead>
                  <tr className="border-b">
                    <th className="text-left py-2 font-medium">Source</th>
                    <th className="text-right py-2 font-medium">Total</th>
                    <th className="text-right py-2 font-medium">Success</th>
                    <th className="text-right py-2 font-medium">Failed</th>
                    <th className="text-right py-2 font-medium">Merged</th>
                    <th className="text-right py-2 font-medium">Closed</th>
                    <th className="text-right py-2 font-medium">Cannot Fix</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(stats.by_source).map(([name, sourceStats]) => (
                    <tr key={name} className="border-b last:border-0">
                      <td className="py-2 font-medium capitalize">{name}</td>
                      <td className="text-right py-2">{sourceStats.total}</td>
                      <td className="text-right py-2 text-green-600">{sourceStats.success}</td>
                      <td className="text-right py-2 text-red-600">{sourceStats.failed}</td>
                      <td className="text-right py-2 text-purple-600">{sourceStats.merged}</td>
                      <td className="text-right py-2 text-orange-600">{sourceStats.closed}</td>
                      <td className="text-right py-2 text-gray-600">{sourceStats.cannot_fix}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </CardContent>
        </Card>
      )}
    </div>
  )
}

function StatusCard({ status, count }: { status: string; count: number }) {
  const config: Record<string, { color: string; icon: React.ReactNode }> = {
    pending: { color: 'text-yellow-500', icon: <Clock className="h-4 w-4" /> },
    success: { color: 'text-green-500', icon: <CheckCircle2 className="h-4 w-4" /> },
    merged: { color: 'text-purple-500', icon: <GitMerge className="h-4 w-4" /> },
    closed: { color: 'text-orange-500', icon: <XCircle className="h-4 w-4" /> },
    failed: { color: 'text-red-500', icon: <XCircle className="h-4 w-4" /> },
    cannot_fix: { color: 'text-gray-500', icon: <AlertTriangle className="h-4 w-4" /> },
  }

  const { color, icon } = config[status] || { color: 'text-gray-500', icon: null }

  return (
    <Card>
      <CardContent className="pt-6">
        <div className="flex items-center justify-between">
          <span className={`text-sm font-medium capitalize ${color}`}>{status.replace('_', ' ')}</span>
          <span className={color}>{icon}</span>
        </div>
        <div className="text-2xl font-bold mt-2">{count}</div>
      </CardContent>
    </Card>
  )
}

function AttemptRow({ attempt }: { attempt: AttemptSummary }) {
  const date = new Date(attempt.attempted_at)
  const timeAgo = getTimeAgo(date)

  return (
    <div className="flex items-center justify-between py-2 border-b last:border-0">
      <div className="flex items-center gap-3">
        <StatusBadge status={attempt.status} />
        <div>
          <p className="font-medium">{attempt.short_id}</p>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <span className="capitalize">{attempt.source}</span>
            {attempt.retry_count > 0 && (
              <span className="flex items-center gap-1">
                <RefreshCw className="h-3 w-3" />
                {attempt.retry_count}
              </span>
            )}
          </div>
        </div>
      </div>
      <div className="flex items-center gap-2">
        <span className="text-sm text-muted-foreground">{timeAgo}</span>
        {attempt.pr_url && (
          <a href={attempt.pr_url} target="_blank" rel="noopener noreferrer" className="text-primary hover:text-primary/80">
            <ExternalLink className="h-4 w-4" />
          </a>
        )}
      </div>
    </div>
  )
}

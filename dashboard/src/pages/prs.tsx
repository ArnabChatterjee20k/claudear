import { useState } from 'react'
import useSWR from 'swr'
import {
  fetchPrAnalytics,
  fetchPrs,
  type PrAnalytics,
  type PrRecord,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { StatusBadge } from '../components/shared/status-badge'
import { DataTable, type Column } from '../components/shared/data-table'
import { Select } from '../components/ui/select'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import {
  GitPullRequest,
  GitMerge,
  XCircle,
  Clock,
  TrendingUp,
  ExternalLink,
} from 'lucide-react'

function formatMins(mins: number | null): string {
  if (mins == null) return '--'
  if (mins < 60) return `${mins.toFixed(0)}m`
  if (mins < 1440) return `${(mins / 60).toFixed(1)}h`
  return `${(mins / 1440).toFixed(1)}d`
}

const statusOptions = [
  { value: 'open', label: 'Open' },
  { value: 'merged', label: 'Merged' },
  { value: 'closed', label: 'Closed' },
]

export default function PrsPage() {
  const [statusFilter, setStatusFilter] = useState('')

  const { data: analytics, isLoading: analyticsLoading } = useSWR<PrAnalytics>(
    'pr-analytics',
    fetchPrAnalytics,
    { refreshInterval: 30000 },
  )

  const { data: prs, error, isLoading: prsLoading } = useSWR<PrRecord[]>(
    ['prs', statusFilter],
    () => fetchPrs({ status: statusFilter || undefined }),
    { refreshInterval: 15000 },
  )

  const columns: Column<PrRecord>[] = [
    {
      key: 'status',
      header: 'Status',
      render: row => <StatusBadge status={row.status} />,
    },
    {
      key: 'pr_number',
      header: 'PR #',
      render: row => (
        <a
          href={row.pr_url}
          target="_blank"
          rel="noopener noreferrer"
          className="text-primary hover:underline inline-flex items-center gap-1"
        >
          #{row.pr_number}
          <ExternalLink className="h-3 w-3" />
        </a>
      ),
    },
    {
      key: 'github_repo',
      header: 'Repo',
      render: row => <span className="text-sm">{row.github_repo}</span>,
    },
    {
      key: 'title',
      header: 'Title',
      render: row => (
        <span className="text-sm" title={row.title || undefined}>
          {row.title && row.title.length > 60
            ? row.title.slice(0, 60) + '...'
            : row.title || '--'}
        </span>
      ),
      className: 'max-w-sm',
    },
    {
      key: 'author',
      header: 'Author',
      render: row => <span className="text-sm">{row.author || '--'}</span>,
    },
    {
      key: 'reviews',
      header: 'Reviews',
      render: row => (
        <span className="text-sm">
          <span className="text-green-600">{row.approvals_count}</span>
          {' / '}
          <span className="text-red-600">{row.changes_requested_count}</span>
        </span>
      ),
    },
    {
      key: 'time_to_merge_mins',
      header: 'Time to Merge',
      render: row => (
        <span className="text-sm">{formatMins(row.time_to_merge_mins)}</span>
      ),
      sortable: true,
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Pull Requests" description="PR lifecycle and review analytics" />

      {analyticsLoading && (
        <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-6">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-28" />
          ))}
        </div>
      )}

      {analytics && (
        <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-6">
          <StatsCard
            title="Total PRs"
            value={analytics.total}
            icon={<GitPullRequest className="h-4 w-4 text-blue-500" />}
            description="All tracked PRs"
          />
          <StatsCard
            title="Open"
            value={analytics.open}
            icon={<GitPullRequest className="h-4 w-4 text-green-500" />}
            description="Currently open"
          />
          <StatsCard
            title="Merged"
            value={analytics.merged}
            icon={<GitMerge className="h-4 w-4 text-purple-500" />}
            description="Successfully merged"
          />
          <StatsCard
            title="Closed"
            value={analytics.closed}
            icon={<XCircle className="h-4 w-4 text-orange-500" />}
            description="Closed without merge"
          />
          <StatsCard
            title="Avg Merge Time"
            value={formatMins(analytics.avg_time_to_merge_mins ?? null)}
            icon={<Clock className="h-4 w-4 text-yellow-500" />}
            description="Mean time to merge"
          />
          <StatsCard
            title="Merge Rate"
            value={
              analytics.merge_rate != null
                ? `${analytics.merge_rate.toFixed(1)}%`
                : '--'
            }
            icon={<TrendingUp className="h-4 w-4 text-green-500" />}
            description="Merged / total"
          />
        </div>
      )}

      <div className="flex items-center gap-4">
        <Select
          value={statusFilter}
          onChange={setStatusFilter}
          options={statusOptions}
          placeholder="All statuses"
          className="w-40"
        />
      </div>

      {error && (
        <div className="text-destructive text-sm">Failed to load pull requests.</div>
      )}

      {prsLoading && (
        <div className="space-y-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      )}

      {prs && (
        <Card>
          <CardContent className="p-4">
            <DataTable
              columns={columns}
              data={prs}
              keyFn={row => row.id}
              emptyMessage="No pull requests found"
            />
          </CardContent>
        </Card>
      )}
    </div>
  )
}

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
import { Modal } from '../components/shared/modal'
import { Select } from '../components/ui/select'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { formatMins, formatDate } from '../lib/formatters'
import { BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer } from 'recharts'
import {
  GitPullRequest,
  GitMerge,
  XCircle,
  Clock,
  TrendingUp,
  ExternalLink,
} from 'lucide-react'

const statusOptions = [
  { value: 'open', label: 'Open' },
  { value: 'merged', label: 'Merged' },
  { value: 'closed', label: 'Closed' },
]

export default function PrsPage() {
  const [statusFilter, setStatusFilter] = useState('')
  const [selectedPr, setSelectedPr] = useState<PrRecord | null>(null)

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
          onClick={e => e.stopPropagation()}
        >
          #{row.pr_number}
          <ExternalLink className="h-3 w-3" />
        </a>
      ),
    },
    {
      key: 'scm_repo',
      header: 'Repo',
      render: row => <span className="text-sm">{row.scm_repo}</span>,
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
        <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-7">
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
          <StatsCard
            title="Avg Time to PR"
            value={formatMins(analytics.avg_time_to_pr_mins ?? null)}
            icon={<Clock className="h-4 w-4 text-indigo-500" />}
            description="Issue → PR creation"
          />
        </div>
      )}

      {analytics && (analytics.rejection_reasons ?? []).length > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>PR Rejection Reasons</CardTitle>
            <CardDescription>Top review-change categories by frequency</CardDescription>
          </CardHeader>
          <CardContent>
            <div className="h-64">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={analytics.rejection_reasons} layout="vertical">
                  <CartesianGrid strokeDasharray="3 3" className="stroke-muted" />
                  <XAxis type="number" tick={{ fontSize: 12 }} className="text-muted-foreground" />
                  <YAxis type="category" dataKey="category" tick={{ fontSize: 12 }} className="text-muted-foreground" width={150} />
                  <Tooltip />
                  <Bar dataKey="count" fill="#f97316" radius={[0, 4, 4, 0]} name="Count" />
                </BarChart>
              </ResponsiveContainer>
            </div>
          </CardContent>
        </Card>
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
              onRowClick={row => setSelectedPr(row)}
            />
          </CardContent>
        </Card>
      )}

      <Modal
        open={!!selectedPr}
        onClose={() => setSelectedPr(null)}
        title={selectedPr ? `PR #${selectedPr.pr_number}` : undefined}
      >
        {selectedPr && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <p className="text-sm text-muted-foreground">Status</p>
                <StatusBadge status={selectedPr.status} />
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Repository</p>
                <p className="text-sm">{selectedPr.scm_repo}</p>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Title</p>
                <p className="text-sm">{selectedPr.title || '--'}</p>
              </div>
              {selectedPr.description && (
                <div className="sm:col-span-2">
                  <p className="text-sm text-muted-foreground">Description</p>
                  <p className="text-sm whitespace-pre-wrap">{selectedPr.description}</p>
                </div>
              )}
              <div>
                <p className="text-sm text-muted-foreground">Author</p>
                <p className="text-sm">{selectedPr.author || '--'}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Branch</p>
                <p className="text-sm font-mono">
                  {selectedPr.head_branch || '--'} → {selectedPr.base_branch || '--'}
                </p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Files Changed</p>
                <p className="text-sm">{selectedPr.files_changed ?? '--'}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Lines</p>
                <p className="text-sm">
                  <span className="text-green-600">+{selectedPr.lines_added ?? 0}</span>
                  {' / '}
                  <span className="text-red-600">-{selectedPr.lines_removed ?? 0}</span>
                </p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Approvals / Changes Requested</p>
                <p className="text-sm">
                  <span className="text-green-600">{selectedPr.approvals_count}</span>
                  {' / '}
                  <span className="text-red-600">{selectedPr.changes_requested_count}</span>
                </p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Review Cycles</p>
                <p className="text-sm">{selectedPr.review_cycles}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Time to First Review</p>
                <p className="text-sm">{formatMins(selectedPr.time_to_first_review_mins)}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Time to Merge</p>
                <p className="text-sm">{formatMins(selectedPr.time_to_merge_mins)}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Created</p>
                <p className="text-sm">{formatDate(selectedPr.created_at)}</p>
              </div>
              {selectedPr.merged_at && (
                <div>
                  <p className="text-sm text-muted-foreground">Merged</p>
                  <p className="text-sm">{formatDate(selectedPr.merged_at)}</p>
                </div>
              )}
              {selectedPr.closed_at && (
                <div>
                  <p className="text-sm text-muted-foreground">Closed</p>
                  <p className="text-sm">{formatDate(selectedPr.closed_at)}</p>
                </div>
              )}
            </div>
            <div>
              <a
                href={selectedPr.pr_url}
                target="_blank"
                rel="noopener noreferrer"
                className="text-sm text-primary hover:underline inline-flex items-center gap-1"
              >
                <ExternalLink className="h-3 w-3" />
                View on GitHub
              </a>
            </div>
          </div>
        )}
      </Modal>
    </div>
  )
}

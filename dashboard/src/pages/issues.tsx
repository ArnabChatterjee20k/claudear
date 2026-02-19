import { useState, useMemo } from 'react'
import useSWR from 'swr'
import {
  fetchAttempts,
  type AttemptsResponse,
  type AttemptSummary,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { DataTable, type Column } from '../components/shared/data-table'
import { StatusBadge } from '../components/shared/status-badge'
import { TimeAgo } from '../components/shared/time-ago'
import { Modal } from '../components/shared/modal'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { formatDate } from '../lib/formatters'

interface IssueGroup {
  key: string
  source: string
  short_id: string
  title: string
  attempts: AttemptSummary[]
  latest_status: string
  last_attempted_at: string
}

function groupAttemptsByIssue(attempts: AttemptSummary[]): IssueGroup[] {
  const map = new Map<string, IssueGroup>()
  for (const a of attempts) {
    const key = `${a.source}:${a.short_id}`
    const existing = map.get(key)
    if (existing) {
      existing.attempts.push(a)
      if (new Date(a.attempted_at) > new Date(existing.last_attempted_at)) {
        existing.latest_status = a.status
        existing.last_attempted_at = a.attempted_at
        existing.title = a.title || existing.title
      }
    } else {
      map.set(key, {
        key,
        source: a.source,
        short_id: a.short_id,
        title: a.title || a.short_id,
        attempts: [a],
        latest_status: a.status,
        last_attempted_at: a.attempted_at,
      })
    }
  }
  return Array.from(map.values()).sort(
    (a, b) => new Date(b.last_attempted_at).getTime() - new Date(a.last_attempted_at).getTime(),
  )
}

export default function IssuesPage() {
  const [selectedIssue, setSelectedIssue] = useState<IssueGroup | null>(null)

  const { data, error, isLoading } = useSWR<AttemptsResponse>(
    'issues-all-attempts',
    () => fetchAttempts({ per_page: 1000 }),
    { refreshInterval: 30000 },
  )

  const issues = useMemo(
    () => (data ? groupAttemptsByIssue(data.attempts) : []),
    [data],
  )

  const columns: Column<IssueGroup>[] = [
    {
      key: 'source',
      header: 'Source',
      render: row => (
        <span className="px-2 py-0.5 rounded text-xs font-medium bg-blue-100 text-blue-800 capitalize">
          {row.source}
        </span>
      ),
    },
    {
      key: 'short_id',
      header: 'Issue ID',
      render: row => <span className="font-mono text-sm">{row.short_id}</span>,
    },
    {
      key: 'title',
      header: 'Title',
      render: row => (
        <span className="text-sm" title={row.title}>
          {row.title.length > 60 ? row.title.slice(0, 60) + '...' : row.title}
        </span>
      ),
      className: 'max-w-sm',
    },
    {
      key: 'attempts_count',
      header: 'Attempts',
      render: row => <span className="text-sm font-medium">{row.attempts.length}</span>,
      sortable: true,
    },
    {
      key: 'latest_status',
      header: 'Latest Status',
      render: row => <StatusBadge status={row.latest_status} />,
    },
    {
      key: 'last_attempted_at',
      header: 'Last Attempted',
      render: row => <TimeAgo date={row.last_attempted_at} />,
      sortable: true,
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Issues" description="Originating issues grouped from fix attempts" />

      {error && (
        <div className="text-destructive text-sm">Failed to load issues.</div>
      )}

      {isLoading && (
        <div className="space-y-2">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      )}

      {data && (
        <Card>
          <CardContent className="p-4">
            <DataTable
              columns={columns}
              data={issues}
              keyFn={row => row.key}
              emptyMessage="No issues found"
              onRowClick={row => setSelectedIssue(row)}
            />
          </CardContent>
        </Card>
      )}

      <Modal
        open={!!selectedIssue}
        onClose={() => setSelectedIssue(null)}
        title={selectedIssue ? `${selectedIssue.source}: ${selectedIssue.short_id}` : undefined}
      >
        {selectedIssue && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <p className="text-sm text-muted-foreground">Source</p>
                <p className="text-sm capitalize">{selectedIssue.source}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Issue ID</p>
                <p className="text-sm font-mono">{selectedIssue.short_id}</p>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Title</p>
                <p className="text-sm">{selectedIssue.title}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Latest Status</p>
                <StatusBadge status={selectedIssue.latest_status} />
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Total Attempts</p>
                <p className="text-sm font-medium">{selectedIssue.attempts.length}</p>
              </div>
            </div>

            <div>
              <p className="text-sm font-medium mb-2">Attempts</p>
              <div className="space-y-2">
                {selectedIssue.attempts
                  .sort((a, b) => new Date(b.attempted_at).getTime() - new Date(a.attempted_at).getTime())
                  .map(a => (
                    <div key={a.id} className="flex items-center gap-3 border rounded-md p-2 text-sm">
                      <StatusBadge status={a.status} />
                      <span className="text-muted-foreground">{formatDate(a.attempted_at)}</span>
                      {a.pr_url && (
                        <a
                          href={a.pr_url}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="text-primary hover:underline text-xs"
                        >
                          PR
                        </a>
                      )}
                      {a.retry_count > 0 && (
                        <span className="text-xs text-muted-foreground">
                          retry #{a.retry_count}
                        </span>
                      )}
                    </div>
                  ))}
              </div>
            </div>
          </div>
        )}
      </Modal>
    </div>
  )
}

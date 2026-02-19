import { useState } from 'react'
import useSWR from 'swr'
import { fetchErrors, type ErrorPattern } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Badge } from '../components/ui/badge'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { DataTable, type Column } from '../components/shared/data-table'
import { TimeAgo } from '../components/shared/time-ago'
import { Modal } from '../components/shared/modal'
import { formatDate } from '../lib/formatters'

function truncate(text: string | null, maxLen: number): string {
  if (!text) return '--'
  return text.length > maxLen ? text.slice(0, maxLen) + '...' : text
}

export default function ErrorsPage() {
  const [selectedError, setSelectedError] = useState<ErrorPattern | null>(null)

  const { data, error, isLoading } = useSWR<ErrorPattern[]>(
    'errors',
    () => fetchErrors(50),
    { refreshInterval: 30000 },
  )

  const sorted = data
    ? [...data].sort((a, b) => b.occurrence_count - a.occurrence_count)
    : []

  const columns: Column<ErrorPattern>[] = [
    {
      key: 'error_type',
      header: 'Error Type',
      render: row => (
        <Badge variant="destructive">{row.error_type || 'unknown'}</Badge>
      ),
    },
    {
      key: 'error_message',
      header: 'Message',
      render: row => (
        <span className="text-sm" title={row.error_message || undefined}>
          {truncate(row.error_message, 80)}
        </span>
      ),
      className: 'max-w-md',
    },
    {
      key: 'occurrence_count',
      header: 'Count',
      render: row => <span className="font-medium">{row.occurrence_count}</span>,
      sortable: true,
    },
    {
      key: 'first_seen',
      header: 'First Seen',
      render: row => <TimeAgo date={row.first_seen} />,
      sortable: true,
    },
    {
      key: 'last_seen',
      header: 'Last Seen',
      render: row => <TimeAgo date={row.last_seen} />,
      sortable: true,
    },
    {
      key: 'sources',
      header: 'Sources',
      render: row => (
        <span className="text-sm capitalize">
          {row.sources ? row.sources.join(', ') : '--'}
        </span>
      ),
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Error Patterns" description="Recurring error analysis" />

      {error && (
        <div className="text-destructive text-sm">Failed to load error patterns.</div>
      )}

      {isLoading && (
        <div className="space-y-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      )}

      {sorted && (
        <Card>
          <CardContent className="p-4">
            <DataTable
              columns={columns}
              data={sorted}
              keyFn={row => row.id}
              emptyMessage="No error patterns detected"
              onRowClick={row => setSelectedError(row)}
            />
          </CardContent>
        </Card>
      )}

      <Modal
        open={!!selectedError}
        onClose={() => setSelectedError(null)}
        title={selectedError?.error_type || 'Error Detail'}
      >
        {selectedError && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <p className="text-sm text-muted-foreground">Error Type</p>
                <Badge variant="destructive">{selectedError.error_type || 'unknown'}</Badge>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Occurrences</p>
                <p className="text-sm font-medium">{selectedError.occurrence_count}</p>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Error Message</p>
                <p className="text-sm whitespace-pre-wrap">{selectedError.error_message || '--'}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">First Seen</p>
                <p className="text-sm">{formatDate(selectedError.first_seen)}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Last Seen</p>
                <p className="text-sm">{formatDate(selectedError.last_seen)}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Sources</p>
                <p className="text-sm capitalize">{selectedError.sources?.join(', ') || '--'}</p>
              </div>
              {selectedError.example_issue_ids && selectedError.example_issue_ids.length > 0 && (
                <div>
                  <p className="text-sm text-muted-foreground">Example Issue IDs</p>
                  <div className="flex flex-wrap gap-1 mt-1">
                    {selectedError.example_issue_ids.map(id => (
                      <Badge key={id} variant="secondary">{id}</Badge>
                    ))}
                  </div>
                </div>
              )}
              {selectedError.resolution_hints && (
                <div className="sm:col-span-2">
                  <p className="text-sm text-muted-foreground">Resolution Hints</p>
                  <p className="text-sm whitespace-pre-wrap">{selectedError.resolution_hints}</p>
                </div>
              )}
            </div>
          </div>
        )}
      </Modal>
    </div>
  )
}

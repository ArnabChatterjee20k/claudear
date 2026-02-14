import useSWR from 'swr'
import { fetchErrors, type ErrorPattern } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Badge } from '../components/ui/badge'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { DataTable, type Column } from '../components/shared/data-table'
import { TimeAgo } from '../components/shared/time-ago'

function truncate(text: string | null, maxLen: number): string {
  if (!text) return '--'
  return text.length > maxLen ? text.slice(0, maxLen) + '...' : text
}

export default function ErrorsPage() {
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
    {
      key: 'resolution_hints',
      header: 'Resolution Hints',
      render: row => (
        <span className="text-sm text-muted-foreground">
          {truncate(row.resolution_hints, 60)}
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
            />
          </CardContent>
        </Card>
      )}
    </div>
  )
}

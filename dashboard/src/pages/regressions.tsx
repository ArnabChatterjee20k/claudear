import { useState } from 'react'
import useSWR from 'swr'
import {
  fetchRegressions,
  fetchRegressionChecks,
  type RegressionWatch,
  type RegressionCheck,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Tabs } from '../components/ui/tabs'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { DataTable, type Column } from '../components/shared/data-table'
import { StatusBadge } from '../components/shared/status-badge'
import { TimeAgo } from '../components/shared/time-ago'

const tabItems = [
  { value: 'awaiting_release', label: 'Awaiting Release' },
  { value: 'monitoring', label: 'Monitoring' },
  { value: 'resolved', label: 'Resolved' },
  { value: 'regressed', label: 'Regressed' },
]

function RegressionTab({ status }: { status: string }) {
  const [selectedWatchId, setSelectedWatchId] = useState<number | null>(null)

  const { data, error, isLoading } = useSWR<RegressionWatch[]>(
    ['regressions', status],
    () => fetchRegressions(status),
    { refreshInterval: 15000 },
  )

  const { data: checks, isLoading: checksLoading } = useSWR<RegressionCheck[]>(
    selectedWatchId ? ['regression-checks', selectedWatchId] : null,
    () => fetchRegressionChecks(selectedWatchId!),
  )

  const columns: Column<RegressionWatch>[] = [
    {
      key: 'issue_id',
      header: 'Issue ID',
      render: row => (
        <button
          onClick={() =>
            setSelectedWatchId(row.id === selectedWatchId ? null : row.id)
          }
          className="font-mono text-sm text-primary hover:underline"
        >
          {row.issue_id}
        </button>
      ),
    },
    {
      key: 'issue_type',
      header: 'Issue Type',
      render: row => <span className="text-sm capitalize">{row.issue_type}</span>,
    },
    {
      key: 'status',
      header: 'Status',
      render: row => <StatusBadge status={row.status} />,
    },
    {
      key: 'pr_merged_at',
      header: 'PR Merged At',
      render: row =>
        row.pr_merged_at ? <TimeAgo date={row.pr_merged_at} /> : <span className="text-muted-foreground">--</span>,
      sortable: true,
    },
    {
      key: 'monitoring_started_at',
      header: 'Monitoring Started',
      render: row =>
        row.monitoring_started_at ? (
          <TimeAgo date={row.monitoring_started_at} />
        ) : (
          <span className="text-muted-foreground">--</span>
        ),
    },
    {
      key: 'created_at',
      header: 'Created At',
      render: row => <TimeAgo date={row.created_at} />,
      sortable: true,
    },
  ]

  if (error) {
    return <div className="text-destructive text-sm">Failed to load regressions.</div>
  }

  if (isLoading) {
    return (
      <div className="space-y-2">
        {Array.from({ length: 4 }).map((_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    )
  }

  return (
    <div className="space-y-4">
      <Card>
        <CardContent className="p-4">
          <DataTable
            columns={columns}
            data={data || []}
            keyFn={row => row.id}
            emptyMessage={`No ${status.replace(/_/g, ' ')} regressions`}
          />
        </CardContent>
      </Card>

      {selectedWatchId && (
        <Card>
          <CardHeader>
            <CardTitle>Regression Checks Timeline</CardTitle>
          </CardHeader>
          <CardContent>
            {checksLoading && <Skeleton className="h-24 w-full" />}
            {checks && checks.length === 0 && (
              <p className="text-sm text-muted-foreground">No checks recorded yet</p>
            )}
            {checks && checks.length > 0 && (
              <div className="space-y-2">
                {checks.map(check => (
                  <div
                    key={check.id}
                    className="flex items-start gap-3 border-l-2 pl-3 py-1"
                    style={{
                      borderColor: check.issue_still_exists
                        ? 'var(--destructive, #ef4444)'
                        : 'var(--success, #22c55e)',
                    }}
                  >
                    <div className="flex-1">
                      <div className="flex items-center gap-2">
                        <span
                          className={`text-xs font-medium ${
                            check.issue_still_exists
                              ? 'text-red-600'
                              : 'text-green-600'
                          }`}
                        >
                          {check.issue_still_exists ? 'Issue persists' : 'Issue resolved'}
                        </span>
                        {check.checked_at && <TimeAgo date={check.checked_at} />}
                      </div>
                      {check.check_details && (
                        <p className="text-sm text-muted-foreground mt-1">
                          {check.check_details}
                        </p>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      )}
    </div>
  )
}

export default function RegressionsPage() {
  return (
    <div className="space-y-6">
      <PageHeader title="Regressions" description="Bug fix regression monitoring" />

      <Tabs tabs={tabItems} defaultTab="awaiting_release">
        {activeTab => <RegressionTab status={activeTab} />}
      </Tabs>
    </div>
  )
}

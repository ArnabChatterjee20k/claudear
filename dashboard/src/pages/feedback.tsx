import { useState } from 'react'
import useSWR from 'swr'
import { fetchFeedback, type FixOutcome } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Select } from '../components/ui/select'
import { Badge } from '../components/ui/badge'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { DataTable, type Column } from '../components/shared/data-table'
import { Modal } from '../components/shared/modal'

const outcomeColors: Record<string, string> = {
  success: 'bg-green-100 text-green-800',
  failure: 'bg-red-100 text-red-800',
  partial: 'bg-yellow-100 text-yellow-800',
}

const sourceOptions = [
  { value: 'sentry', label: 'Sentry' },
  { value: 'linear', label: 'Linear' },
  { value: 'jira', label: 'Jira' },
  { value: 'github', label: 'GitHub' },
]

export default function FeedbackPage() {
  const [source, setSource] = useState('')
  const [selectedFeedback, setSelectedFeedback] = useState<FixOutcome | null>(null)

  const { data, error, isLoading } = useSWR<FixOutcome[]>(
    ['feedback', source],
    () => fetchFeedback({ source: source || undefined }),
    { refreshInterval: 30000 },
  )

  const columns: Column<FixOutcome>[] = [
    {
      key: 'attempt_id',
      header: 'Attempt ID',
      render: row => <span className="font-mono text-sm">{row.attempt_id}</span>,
    },
    {
      key: 'source',
      header: 'Source',
      render: row => <span className="capitalize text-sm">{row.source}</span>,
    },
    {
      key: 'outcome',
      header: 'Outcome',
      render: row => (
        <span
          className={`px-2 py-0.5 rounded text-xs font-medium ${
            outcomeColors[row.outcome] || 'bg-gray-100 text-gray-800'
          }`}
        >
          {row.outcome}
        </span>
      ),
    },
    {
      key: 'error_type',
      header: 'Error Type',
      render: row => <span className="text-sm">{row.error_type || '--'}</span>,
    },
    {
      key: 'learnings',
      header: 'Learnings',
      render: row => (
        <span className="text-sm" title={row.learnings || undefined}>
          {row.learnings && row.learnings.length > 80
            ? row.learnings.slice(0, 80) + '...'
            : row.learnings || '--'}
        </span>
      ),
      className: 'max-w-sm',
    },
    {
      key: 'keywords',
      header: 'Keywords',
      render: row => (
        <div className="flex flex-wrap gap-1">
          {row.keywords.length > 0
            ? row.keywords.slice(0, 3).map(kw => (
                <Badge key={kw} variant="secondary">
                  {kw}
                </Badge>
              ))
            : '--'}
          {row.keywords.length > 3 && (
            <span className="text-xs text-muted-foreground">+{row.keywords.length - 3}</span>
          )}
        </div>
      ),
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Feedback" description="Fix outcomes and learnings" />

      <div className="flex items-center gap-4">
        <Select
          value={source}
          onChange={setSource}
          options={sourceOptions}
          placeholder="All sources"
          className="w-48"
        />
      </div>

      {error && (
        <div className="text-destructive text-sm">Failed to load feedback data.</div>
      )}

      {isLoading && (
        <div className="space-y-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full" />
          ))}
        </div>
      )}

      {data && (
        <Card>
          <CardContent className="p-4">
            <DataTable
              columns={columns}
              data={data}
              keyFn={row => row.id}
              emptyMessage="No feedback recorded yet"
              onRowClick={row => setSelectedFeedback(row)}
            />
          </CardContent>
        </Card>
      )}

      <Modal
        open={!!selectedFeedback}
        onClose={() => setSelectedFeedback(null)}
        title={selectedFeedback ? `Feedback: Attempt #${selectedFeedback.attempt_id}` : undefined}
      >
        {selectedFeedback && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <p className="text-sm text-muted-foreground">Source</p>
                <p className="text-sm capitalize">{selectedFeedback.source}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Outcome</p>
                <span
                  className={`px-2 py-0.5 rounded text-xs font-medium ${
                    outcomeColors[selectedFeedback.outcome] || 'bg-gray-100 text-gray-800'
                  }`}
                >
                  {selectedFeedback.outcome}
                </span>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Issue ID</p>
                <p className="text-sm font-mono">{selectedFeedback.issue_id}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Error Type</p>
                <p className="text-sm">{selectedFeedback.error_type || '--'}</p>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Issue Text</p>
                <p className="text-sm whitespace-pre-wrap">{selectedFeedback.issue_text || '--'}</p>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Prompt Used</p>
                <pre className="text-xs bg-muted/50 rounded-md p-3 overflow-x-auto whitespace-pre-wrap max-h-48 overflow-y-auto">
                  {selectedFeedback.prompt_used || '--'}
                </pre>
              </div>
              <div className="sm:col-span-2">
                <p className="text-sm text-muted-foreground">Learnings</p>
                <p className="text-sm whitespace-pre-wrap">{selectedFeedback.learnings || '--'}</p>
              </div>
              {selectedFeedback.keywords.length > 0 && (
                <div className="sm:col-span-2">
                  <p className="text-sm text-muted-foreground mb-1">Keywords</p>
                  <div className="flex flex-wrap gap-1">
                    {selectedFeedback.keywords.map(kw => (
                      <Badge key={kw} variant="secondary">
                        {kw}
                      </Badge>
                    ))}
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </Modal>
    </div>
  )
}

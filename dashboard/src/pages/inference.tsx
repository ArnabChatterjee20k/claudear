import { useState } from 'react'
import useSWR from 'swr'
import {
  fetchInferenceStats,
  fetchInferenceHistory,
  type InferenceStats,
  type InferenceHistoryEntry,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { BlockSkeleton, StatsGridSkeleton } from '../components/shared/page-skeletons'
import { StatsCard } from '../components/shared/stats-card'
import { TimeAgo } from '../components/shared/time-ago'
import { DataTable, type Column } from '../components/shared/data-table'
import { Modal } from '../components/shared/modal'
import { Badge } from '../components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { formatDate } from '../lib/formatters'
import {
  Brain,
  Target,
  MessageSquare,
  CheckCircle2,
  BarChart3,
  Check,
  X,
} from 'lucide-react'

const confidenceColors: Record<string, string> = {
  high: 'bg-green-500/10 text-green-700 dark:text-green-400',
  medium: 'bg-yellow-500/10 text-yellow-700 dark:text-yellow-400',
  low: 'bg-orange-500/10 text-orange-700 dark:text-orange-400',
  none: 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-400',
}

export default function InferencePage() {
  const [selectedEntry, setSelectedEntry] = useState<InferenceHistoryEntry | null>(null)

  const { data: stats, isLoading: statsLoading } = useSWR<InferenceStats>(
    'inference-stats',
    fetchInferenceStats,
    { refreshInterval: 30000 },
  )

  const { data: history, error, isLoading: historyLoading } = useSWR<InferenceHistoryEntry[]>(
    'inference-history',
    () => fetchInferenceHistory(50),
    { refreshInterval: 15000 },
  )

  const columns: Column<InferenceHistoryEntry>[] = [
    {
      key: 'issue_id',
      header: 'Issue ID',
      render: row => <span className="font-mono text-sm">{row.issue_id}</span>,
    },
    {
      key: 'issue_source',
      header: 'Source',
      render: row => <span className="text-sm capitalize">{row.issue_source}</span>,
    },
    {
      key: 'inferred_repo_name',
      header: 'Inferred Repo',
      render: row => (
        <span className="text-sm font-medium">
          {row.inferred_repo_name || '--'}
        </span>
      ),
    },
    {
      key: 'confidence',
      header: 'Confidence',
      render: row =>
        row.confidence ? (
          <span
            className={`px-2 py-0.5 rounded text-xs font-medium ${
              confidenceColors[row.confidence] || 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-400'
            }`}
          >
            {row.confidence}
          </span>
        ) : (
          <span className="text-muted-foreground text-sm">--</span>
        ),
    },
    {
      key: 'was_correct',
      header: 'Correct',
      render: row =>
        row.was_correct === true ? (
          <Check className="h-4 w-4 text-green-600" />
        ) : row.was_correct === false ? (
          <X className="h-4 w-4 text-red-600" />
        ) : (
          <span className="text-muted-foreground text-sm">--</span>
        ),
    },
    {
      key: 'duration_ms',
      header: 'Duration',
      render: row => (
        <span className="text-sm">
          {row.duration_ms != null ? `${row.duration_ms}ms` : '--'}
        </span>
      ),
      sortable: true,
    },
    {
      key: 'created_at',
      header: 'Created At',
      render: row => <TimeAgo date={row.created_at} />,
      sortable: true,
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Inference" description="Repository inference accuracy and history" />

      {statsLoading && (
        <StatsGridSkeleton count={6} className="md:grid-cols-3 lg:grid-cols-6" />
      )}

      {stats && (
        <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-6">
          <StatsCard
            title="Accuracy"
            value={`${stats.accuracy.toFixed(1)}%`}
            icon={<Target className="h-4 w-4 text-green-500" />}
            description="Correct inferences"
          />
          <StatsCard
            title="Total Attempts"
            value={stats.total_attempts}
            icon={<Brain className="h-4 w-4 text-primary" />}
            description="All inference runs"
          />
          <StatsCard
            title="With Feedback"
            value={stats.with_feedback}
            icon={<MessageSquare className="h-4 w-4 text-purple-500" />}
            description="Have correctness data"
          />
          <StatsCard
            title="Correct"
            value={stats.correct}
            icon={<CheckCircle2 className="h-4 w-4 text-green-500" />}
            description="Verified correct"
          />
          <StatsCard
            title="High Confidence"
            value={`${stats.by_confidence.high} / ${stats.by_confidence.medium}`}
            icon={<BarChart3 className="h-4 w-4 text-yellow-500" />}
            description="High / Medium"
          />
          <StatsCard
            title="Low / None"
            value={`${stats.by_confidence.low} / ${stats.by_confidence.none}`}
            icon={<BarChart3 className="h-4 w-4 text-gray-500" />}
            description="Low / No confidence"
          />
        </div>
      )}

      {error && (
        <div className="text-destructive text-sm">Failed to load inference history.</div>
      )}

      {historyLoading && <BlockSkeleton className="h-64 w-full" />}

      {history && (
        <Card>
          <CardHeader>
            <CardTitle>Inference History</CardTitle>
          </CardHeader>
          <CardContent>
            <DataTable
              columns={columns}
              data={history}
              keyFn={row => row.id}
              emptyMessage="No inference history recorded"
              onRowClick={row => setSelectedEntry(row)}
            />
          </CardContent>
        </Card>
      )}

      <Modal
        open={!!selectedEntry}
        onClose={() => setSelectedEntry(null)}
        title={selectedEntry ? `Inference: ${selectedEntry.issue_id}` : undefined}
      >
        {selectedEntry && (
          <div className="space-y-4">
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <p className="text-sm text-muted-foreground">Issue ID</p>
                <p className="text-sm font-mono">{selectedEntry.issue_id}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Source</p>
                <p className="text-sm capitalize">{selectedEntry.issue_source}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Inferred Repo</p>
                <p className="text-sm font-medium">{selectedEntry.inferred_repo_name || '--'}</p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Confidence</p>
                {selectedEntry.confidence ? (
                  <span
                    className={`px-2 py-0.5 rounded text-xs font-medium ${
                      confidenceColors[selectedEntry.confidence] || 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-400'
                    }`}
                  >
                    {selectedEntry.confidence}
                  </span>
                ) : (
                  <p className="text-sm text-muted-foreground">--</p>
                )}
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Correct</p>
                <p className="text-sm">
                  {selectedEntry.was_correct === true
                    ? 'Yes'
                    : selectedEntry.was_correct === false
                      ? 'No'
                      : '--'}
                </p>
              </div>
              <div>
                <p className="text-sm text-muted-foreground">Duration</p>
                <p className="text-sm">{selectedEntry.duration_ms != null ? `${selectedEntry.duration_ms}ms` : '--'}</p>
              </div>
              {selectedEntry.inference_reason && (
                <div className="sm:col-span-2">
                  <p className="text-sm text-muted-foreground">Inference Reason</p>
                  <p className="text-sm whitespace-pre-wrap">{selectedEntry.inference_reason}</p>
                </div>
              )}
              {selectedEntry.extracted_keywords && (
                <div className="sm:col-span-2">
                  <p className="text-sm text-muted-foreground mb-1">Keywords</p>
                  <div className="flex flex-wrap gap-1">
                    {selectedEntry.extracted_keywords.split(',').map(kw => kw.trim()).filter(Boolean).map(kw => (
                      <Badge key={kw} variant="secondary">{kw}</Badge>
                    ))}
                  </div>
                </div>
              )}
              <div>
                <p className="text-sm text-muted-foreground">Created</p>
                <p className="text-sm">{formatDate(selectedEntry.created_at)}</p>
              </div>
            </div>
          </div>
        )}
      </Modal>
    </div>
  )
}

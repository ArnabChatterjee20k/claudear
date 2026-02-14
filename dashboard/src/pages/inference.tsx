import useSWR from 'swr'
import {
  fetchInferenceStats,
  fetchInferenceHistory,
  type InferenceStats,
  type InferenceHistoryEntry,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { TimeAgo } from '../components/shared/time-ago'
import { DataTable, type Column } from '../components/shared/data-table'
import { Badge } from '../components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
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
  high: 'bg-green-100 text-green-800',
  medium: 'bg-yellow-100 text-yellow-800',
  low: 'bg-orange-100 text-orange-800',
  none: 'bg-gray-100 text-gray-800',
}

export default function InferencePage() {
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
              confidenceColors[row.confidence] || 'bg-gray-100 text-gray-800'
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
      key: 'extracted_keywords',
      header: 'Keywords',
      render: row => {
        if (!row.extracted_keywords) return <span className="text-muted-foreground text-sm">--</span>
        const keywords = row.extracted_keywords.split(',').map(k => k.trim()).filter(Boolean)
        return (
          <div className="flex flex-wrap gap-1">
            {keywords.slice(0, 3).map(kw => (
              <Badge key={kw} variant="secondary">
                {kw}
              </Badge>
            ))}
            {keywords.length > 3 && (
              <span className="text-xs text-muted-foreground">+{keywords.length - 3}</span>
            )}
          </div>
        )
      },
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
        <div className="grid gap-4 md:grid-cols-3 lg:grid-cols-6">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-28" />
          ))}
        </div>
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
            icon={<Brain className="h-4 w-4 text-blue-500" />}
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

      {historyLoading && <Skeleton className="h-64 w-full" />}

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
            />
          </CardContent>
        </Card>
      )}
    </div>
  )
}

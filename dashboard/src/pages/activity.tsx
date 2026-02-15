import { useState } from 'react'
import useSWR from 'swr'
import { fetchActivity, type ActivityLogEntry } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Select } from '../components/ui/select'
import { Badge } from '../components/ui/badge'
import { Card, CardContent } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { EmptyState } from '../components/shared/empty-state'
import { TimeAgo } from '../components/shared/time-ago'
import { ChevronDown, ChevronRight } from 'lucide-react'

const activityTypeColors: Record<string, string> = {
  issue_received: 'bg-blue-100 text-blue-800',
  attempt_started: 'bg-yellow-100 text-yellow-800',
  attempt_completed: 'bg-green-100 text-green-800',
  attempt_failed: 'bg-red-100 text-red-800',
  decision: 'bg-slate-100 text-slate-800',
  claude_started: 'bg-indigo-100 text-indigo-800',
  claude_completed: 'bg-emerald-100 text-emerald-800',
  claude_failed: 'bg-rose-100 text-rose-800',
  claude_timed_out: 'bg-orange-100 text-orange-800',
  pr_created: 'bg-purple-100 text-purple-800',
  pr_merged: 'bg-purple-200 text-purple-900',
  pr_closed: 'bg-orange-100 text-orange-800',
  review_received: 'bg-cyan-100 text-cyan-800',
  retry_scheduled: 'bg-amber-100 text-amber-800',
}

const sourceOptions = [
  { value: 'sentry', label: 'Sentry' },
  { value: 'linear', label: 'Linear' },
  { value: 'jira', label: 'Jira' },
  { value: 'github', label: 'GitHub' },
  { value: 'claude', label: 'Claude' },
  { value: 'watcher', label: 'Watcher' },
]

export default function ActivityPage() {
  const [source, setSource] = useState('')
  const [expandedIds, setExpandedIds] = useState<Set<number>>(new Set())

  const { data, error, isLoading } = useSWR<ActivityLogEntry[]>(
    ['activity', source],
    () => fetchActivity({ limit: 200, source: source || undefined }),
    { refreshInterval: 5000 },
  )

  const toggleExpanded = (id: number) => {
    setExpandedIds(prev => {
      const next = new Set(prev)
      if (next.has(id)) {
        next.delete(id)
      } else {
        next.add(id)
      }
      return next
    })
  }

  return (
    <div className="space-y-6">
      <PageHeader title="Activity Log" description="Real-time event stream" />

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
        <div className="text-destructive text-sm">Failed to load activity log.</div>
      )}

      {isLoading && (
        <div className="space-y-3">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={i} className="h-16 w-full" />
          ))}
        </div>
      )}

      {data && data.length === 0 && <EmptyState message="No activity recorded yet" />}

      {data && data.length > 0 && (
        <div className="space-y-2">
          {data.map(entry => {
            const isExpanded = expandedIds.has(entry.id)
            const hasMetadata = entry.metadata && Object.keys(entry.metadata).length > 0

            return (
              <Card key={entry.id}>
                <CardContent className="p-4">
                  <div className="flex items-start gap-3">
                    {hasMetadata ? (
                      <button
                        onClick={() => toggleExpanded(entry.id)}
                        className="mt-0.5 text-muted-foreground hover:text-foreground"
                      >
                        {isExpanded ? (
                          <ChevronDown className="h-4 w-4" />
                        ) : (
                          <ChevronRight className="h-4 w-4" />
                        )}
                      </button>
                    ) : (
                      <div className="w-4" />
                    )}

                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 flex-wrap">
                        <TimeAgo date={entry.timestamp} />
                        <span
                          className={`px-2 py-0.5 rounded text-xs font-medium ${
                            activityTypeColors[entry.activity_type] || 'bg-gray-100 text-gray-800'
                          }`}
                        >
                          {entry.activity_type.replace(/_/g, ' ')}
                        </span>
                        {entry.source && (
                          <Badge variant="outline" className="capitalize">
                            {entry.source}
                          </Badge>
                        )}
                        {entry.short_id && (
                          <span className="text-xs font-mono text-muted-foreground">
                            {entry.short_id}
                          </span>
                        )}
                      </div>
                      <p className="text-sm mt-1">{entry.message}</p>

                      {isExpanded && entry.metadata && (
                        <pre className="mt-2 p-3 bg-muted rounded text-xs overflow-x-auto">
                          {JSON.stringify(entry.metadata, null, 2)}
                        </pre>
                      )}
                    </div>
                  </div>
                </CardContent>
              </Card>
            )
          })}
        </div>
      )}
    </div>
  )
}

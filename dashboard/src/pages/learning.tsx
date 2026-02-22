import { useState, useEffect } from 'react'
import useSWR from 'swr'
import {
  fetchRepos,
  fetchRepoLearning,
  type StoredIndexedRepo,
  type RepoLearningResponse,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsGridSkeleton } from '../components/shared/page-skeletons'
import { StatsCard } from '../components/shared/stats-card'
import { DataTable, type Column } from '../components/shared/data-table'
import { Tabs } from '../components/ui/tabs'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { formatDate } from '../lib/formatters'
import {
  BookOpen, ScrollText, Eye, Lightbulb, Link2, ChevronDown,
} from 'lucide-react'

const tabItems = [
  { value: 'knowledge', label: 'Knowledge' },
  { value: 'instructions', label: 'Instructions' },
  { value: 'review_patterns', label: 'Review Patterns' },
  { value: 'strategies', label: 'Strategies' },
  { value: 'diff_analyses', label: 'Diff Analyses' },
  { value: 'correlations', label: 'Correlations' },
]

function ConfidenceBadge({ value }: { value: number }) {
  const pct = Math.round(value * 100)
  const color =
    pct >= 80
      ? 'bg-green-500/10 text-green-700 dark:text-green-400'
      : pct >= 50
        ? 'bg-yellow-500/10 text-yellow-700 dark:text-yellow-400'
        : 'bg-red-500/10 text-red-700 dark:text-red-400'
  return (
    <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${color}`}>
      {pct}%
    </span>
  )
}

function CategoryBadge({ category }: { category: string }) {
  const colors: Record<string, string> = {
    missing_tests: 'bg-orange-500/10 text-orange-700 dark:text-orange-400',
    style_issue: 'bg-purple-500/10 text-purple-700 dark:text-purple-400',
    wrong_approach: 'bg-red-500/10 text-red-700 dark:text-red-400',
    incomplete: 'bg-yellow-500/10 text-yellow-700 dark:text-yellow-400',
    security: 'bg-red-500/10 text-red-700 dark:text-red-400',
    performance: 'bg-blue-500/10 text-blue-700 dark:text-blue-400',
    documentation: 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-400',
  }
  const c = colors[category] || 'bg-zinc-500/10 text-zinc-700 dark:text-zinc-400'
  return (
    <span className={`inline-flex items-center px-2 py-0.5 rounded text-xs font-medium ${c}`}>
      {category.replace(/_/g, ' ')}
    </span>
  )
}

export default function LearningPage() {
  const [selectedRepo, setSelectedRepo] = useState<string>('')

  // Read initial repo from URL query param
  useEffect(() => {
    const params = new URLSearchParams(window.location.search)
    const repo = params.get('repo')
    if (repo) setSelectedRepo(repo)
  }, [])

  const { data: repos } = useSWR<StoredIndexedRepo[]>('repos', fetchRepos, {
    refreshInterval: 60000,
  })

  const { data: learning, isLoading, error } = useSWR<RepoLearningResponse>(
    selectedRepo ? `repo-learning-${selectedRepo}` : null,
    () => fetchRepoLearning(selectedRepo),
    { refreshInterval: 60000 },
  )

  const hasData =
    learning &&
    (learning.knowledge_total > 0 ||
      learning.instructions.length > 0 ||
      learning.review_patterns.length > 0 ||
      learning.strategies.length > 0 ||
      learning.diff_analyses.length > 0 ||
      learning.correlations.length > 0)

  return (
    <div className="space-y-6">
      <PageHeader title="Learning" description="What claudear has learned about each repo" />

      {/* Repo selector */}
      <div className="relative w-full max-w-sm">
        <select
          value={selectedRepo}
          onChange={e => setSelectedRepo(e.target.value)}
          className="w-full appearance-none rounded-md border bg-background px-3 py-2 pr-8 text-sm focus:outline-none focus:ring-2 focus:ring-primary/30"
        >
          <option value="">Select a repository...</option>
          {repos?.map(r => (
            <option key={r.id} value={r.name}>
              {r.name}
            </option>
          ))}
        </select>
        <ChevronDown className="absolute right-2.5 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground pointer-events-none" />
      </div>

      {!selectedRepo && (
        <div className="text-center py-16 text-muted-foreground text-sm">
          Select a repository above to view learning data.
        </div>
      )}

      {selectedRepo && isLoading && (
        <StatsGridSkeleton count={4} className="md:grid-cols-4" />
      )}

      {selectedRepo && error && (
        <div className="text-center py-8 text-destructive text-sm">
          Failed to load learning data.
        </div>
      )}

      {selectedRepo && learning && !hasData && !isLoading && (
        <div className="text-center py-16 text-muted-foreground text-sm">
          No learning data yet for {selectedRepo}. Learning accumulates as claudear processes issues and reviews PRs.
        </div>
      )}

      {learning && hasData && (
        <>
          {/* Stats cards */}
          <div className="grid gap-4 md:grid-cols-4">
            <StatsCard
              title="Knowledge Items"
              value={learning.knowledge_total}
              icon={<BookOpen className="h-4 w-4 text-primary" />}
              description={`${learning.knowledge.length} categories`}
            />
            <StatsCard
              title="Standing Instructions"
              value={learning.instructions.length}
              icon={<ScrollText className="h-4 w-4 text-green-500" />}
              description={`${learning.instructions.filter(i => i.is_active).length} active`}
            />
            <StatsCard
              title="Review Patterns"
              value={learning.review_pattern_summary.total_patterns}
              icon={<Eye className="h-4 w-4 text-purple-500" />}
              description={`${learning.review_pattern_summary.promoted_count} promoted`}
            />
            <StatsCard
              title="Strategies Learned"
              value={learning.strategies.length}
              icon={<Lightbulb className="h-4 w-4 text-yellow-500" />}
              description="Successful fix approaches"
            />
          </div>

          {/* Tabbed content */}
          <Tabs tabs={tabItems} defaultTab="knowledge">
            {activeTab => {
              if (activeTab === 'knowledge') {
                return (
                  <div className="grid gap-4 md:grid-cols-2">
                    {learning.knowledge.map(group => (
                      <Card key={group.key}>
                        <CardHeader className="pb-3">
                          <CardTitle className="text-sm font-medium">{group.label}</CardTitle>
                        </CardHeader>
                        <CardContent className="space-y-2">
                          {group.entries.map(entry => (
                            <div key={entry.id} className="flex items-start justify-between gap-2 text-sm">
                              <span className="text-foreground break-all">{entry.value}</span>
                              <div className="flex items-center gap-2 shrink-0">
                                <ConfidenceBadge value={entry.confidence} />
                                <span className="text-xs text-muted-foreground">{entry.occurrence_count}x</span>
                              </div>
                            </div>
                          ))}
                        </CardContent>
                      </Card>
                    ))}
                  </div>
                )
              }

              if (activeTab === 'instructions') {
                const cols: Column<(typeof learning.instructions)[0]>[] = [
                  {
                    key: 'instruction_text',
                    header: 'Instruction',
                    render: row => <span className="text-sm">{row.instruction_text}</span>,
                  },
                  {
                    key: 'confidence',
                    header: 'Confidence',
                    render: row => <ConfidenceBadge value={row.confidence} />,
                    sortable: true,
                  },
                  {
                    key: 'occurrence_count',
                    header: 'Occurrences',
                    render: row => <span className="text-sm">{row.occurrence_count}</span>,
                    sortable: true,
                  },
                  {
                    key: 'is_active',
                    header: 'Active',
                    render: row =>
                      row.is_active ? (
                        <span className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-green-500/10 text-green-700 dark:text-green-400">
                          active
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">inactive</span>
                      ),
                  },
                  {
                    key: 'updated_at',
                    header: 'Updated',
                    render: row => <span className="text-sm text-muted-foreground">{formatDate(row.updated_at)}</span>,
                    sortable: true,
                  },
                ]
                return (
                  <DataTable
                    columns={cols}
                    data={learning.instructions}
                    keyFn={row => row.id}
                    emptyMessage="No promoted instructions yet"
                  />
                )
              }

              if (activeTab === 'review_patterns') {
                const summary = learning.review_pattern_summary
                const cols: Column<(typeof learning.review_patterns)[0]>[] = [
                  {
                    key: 'category',
                    header: 'Category',
                    render: row => <CategoryBadge category={row.category} />,
                  },
                  {
                    key: 'pattern_text',
                    header: 'Pattern',
                    render: row => <span className="text-sm">{row.pattern_text}</span>,
                  },
                  {
                    key: 'occurrence_count',
                    header: 'Occurrences',
                    render: row => <span className="text-sm">{row.occurrence_count}</span>,
                    sortable: true,
                  },
                  {
                    key: 'promoted',
                    header: 'Promoted',
                    render: row =>
                      row.promoted_to_instruction ? (
                        <span className="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium bg-blue-500/10 text-blue-700 dark:text-blue-400">
                          promoted
                        </span>
                      ) : (
                        <span className="text-xs text-muted-foreground">--</span>
                      ),
                  },
                ]
                return (
                  <div className="space-y-4">
                    {/* Category summary bar */}
                    <div className="flex flex-wrap gap-2">
                      {Object.entries(summary.by_category).map(([cat, count]) => (
                        <div key={cat} className="flex items-center gap-1.5">
                          <CategoryBadge category={cat} />
                          <span className="text-xs text-muted-foreground">{count}</span>
                        </div>
                      ))}
                    </div>
                    <DataTable
                      columns={cols}
                      data={learning.review_patterns}
                      keyFn={row => row.id}
                      emptyMessage="No review patterns yet"
                    />
                  </div>
                )
              }

              if (activeTab === 'strategies') {
                return (
                  <div className="grid gap-4 md:grid-cols-2">
                    {learning.strategies.length === 0 && (
                      <p className="text-sm text-muted-foreground col-span-full text-center py-8">
                        No strategies recorded yet.
                      </p>
                    )}
                    {learning.strategies.map(s => (
                      <Card key={s.id}>
                        <CardHeader className="pb-2">
                          <CardTitle className="text-sm font-medium">{s.fix_approach}</CardTitle>
                        </CardHeader>
                        <CardContent className="space-y-2 text-sm">
                          <p className="text-muted-foreground">{s.strategy_summary}</p>
                          <div className="flex flex-wrap gap-3 text-xs text-muted-foreground">
                            <span>{s.files_explored.length} files explored</span>
                            <span>{s.tests_run} tests run</span>
                            {s.fix_quality_score != null && (
                              <span>Quality: {(s.fix_quality_score * 100).toFixed(0)}%</span>
                            )}
                          </div>
                        </CardContent>
                      </Card>
                    ))}
                  </div>
                )
              }

              if (activeTab === 'diff_analyses') {
                const cols: Column<(typeof learning.diff_analyses)[0]>[] = [
                  {
                    key: 'pr_number',
                    header: 'PR',
                    render: row => (
                      <a
                        href={row.pr_url}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="text-primary hover:underline text-sm"
                      >
                        #{row.pr_number}
                      </a>
                    ),
                  },
                  {
                    key: 'files_changed',
                    header: 'Files Changed',
                    render: row => <span className="text-sm">{row.files_changed.length}</span>,
                    sortable: true,
                  },
                  {
                    key: 'file_types',
                    header: 'File Types',
                    render: row => (
                      <span className="text-sm text-muted-foreground">
                        {Object.keys(row.file_types).join(', ') || '--'}
                      </span>
                    ),
                  },
                  {
                    key: 'categories',
                    header: 'Categories',
                    render: row => (
                      <div className="flex flex-wrap gap-1">
                        {row.change_categories.map(c => (
                          <span
                            key={c}
                            className="inline-flex items-center px-1.5 py-0.5 rounded text-xs bg-muted text-muted-foreground"
                          >
                            {c}
                          </span>
                        ))}
                      </div>
                    ),
                  },
                  {
                    key: 'diff_summary',
                    header: 'Summary',
                    render: row => (
                      <span className="text-sm text-muted-foreground line-clamp-2">{row.diff_summary}</span>
                    ),
                  },
                ]
                return (
                  <DataTable
                    columns={cols}
                    data={learning.diff_analyses}
                    keyFn={row => row.id}
                    emptyMessage="No diff analyses yet"
                  />
                )
              }

              if (activeTab === 'correlations') {
                if (learning.correlations.length === 0) {
                  return (
                    <p className="text-sm text-muted-foreground text-center py-8">
                      No cross-repo correlations detected yet.
                    </p>
                  )
                }
                return (
                  <div className="grid gap-4 md:grid-cols-2 lg:grid-cols-3">
                    {learning.correlations.map(c => (
                      <Card key={c.id}>
                        <CardContent className="pt-6 space-y-2">
                          <div className="flex items-center gap-2 text-sm font-medium">
                            <span>{c.repo_a}</span>
                            <Link2 className="h-3.5 w-3.5 text-muted-foreground" />
                            <span>{c.repo_b}</span>
                          </div>
                          <div className="flex gap-4 text-xs text-muted-foreground">
                            <span>{c.correlation_count} co-occurrences</span>
                            <span>{c.window_hours}h window</span>
                          </div>
                        </CardContent>
                      </Card>
                    ))}
                  </div>
                )
              }

              return null
            }}
          </Tabs>
        </>
      )}
    </div>
  )
}

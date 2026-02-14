import useSWR from 'swr'
import {
  fetchRepos,
  fetchRepoStats,
  fetchDependencies,
  type StoredIndexedRepo,
  type IndexStats,
  type StoredDependency,
} from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { StatsCard } from '../components/shared/stats-card'
import { TimeAgo } from '../components/shared/time-ago'
import { DataTable, type Column } from '../components/shared/data-table'
import { Card, CardContent, CardHeader, CardTitle } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { Database, FileText, Clock, ExternalLink } from 'lucide-react'

export default function ReposPage() {
  const { data: repos, isLoading: reposLoading } = useSWR<StoredIndexedRepo[]>(
    'repos',
    fetchRepos,
    { refreshInterval: 30000 },
  )

  const { data: stats, isLoading: statsLoading } = useSWR<IndexStats>(
    'repo-stats',
    fetchRepoStats,
    { refreshInterval: 30000 },
  )

  const { data: deps, isLoading: depsLoading } = useSWR<StoredDependency[]>(
    'dependencies',
    fetchDependencies,
    { refreshInterval: 60000 },
  )

  const repoColumns: Column<StoredIndexedRepo>[] = [
    {
      key: 'name',
      header: 'Name',
      render: row => <span className="font-medium">{row.name}</span>,
    },
    {
      key: 'path',
      header: 'Path',
      render: row => (
        <span className="text-sm font-mono text-muted-foreground">{row.path}</span>
      ),
    },
    {
      key: 'github_url',
      header: 'GitHub URL',
      render: row =>
        row.github_url ? (
          <a
            href={row.github_url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary hover:underline inline-flex items-center gap-1 text-sm"
          >
            <ExternalLink className="h-3 w-3" />
            Link
          </a>
        ) : (
          <span className="text-muted-foreground text-sm">--</span>
        ),
    },
    {
      key: 'default_branch',
      header: 'Default Branch',
      render: row => <span className="text-sm">{row.default_branch}</span>,
    },
    {
      key: 'file_count',
      header: 'File Count',
      render: row => <span className="text-sm">{row.file_count}</span>,
      sortable: true,
    },
    {
      key: 'last_indexed_at',
      header: 'Last Indexed',
      render: row => <TimeAgo date={row.last_indexed_at} />,
      sortable: true,
    },
  ]

  const depColumns: Column<StoredDependency>[] = [
    {
      key: 'upstream',
      header: 'Upstream',
      render: row => <span className="font-medium text-sm">{row.upstream}</span>,
    },
    {
      key: 'downstream',
      header: 'Downstream',
      render: row => <span className="font-medium text-sm">{row.downstream}</span>,
    },
    {
      key: 'dep_type',
      header: 'Type',
      render: row => <span className="text-sm capitalize">{row.dep_type}</span>,
    },
  ]

  return (
    <div className="space-y-6">
      <PageHeader title="Repositories" description="Repository index and dependencies" />

      {statsLoading && (
        <div className="grid gap-4 md:grid-cols-3">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-28" />
          ))}
        </div>
      )}

      {stats && (
        <div className="grid gap-4 md:grid-cols-3">
          <StatsCard
            title="Total Repos"
            value={stats.repo_count}
            icon={<Database className="h-4 w-4 text-blue-500" />}
            description="Indexed repositories"
          />
          <StatsCard
            title="Files Indexed"
            value={stats.file_count}
            icon={<FileText className="h-4 w-4 text-green-500" />}
            description="Total indexed files"
          />
          <StatsCard
            title="Last Indexed At"
            value={
              stats.last_indexed_at
                ? new Date(stats.last_indexed_at).toLocaleString()
                : '--'
            }
            icon={<Clock className="h-4 w-4 text-yellow-500" />}
            description="Most recent index run"
          />
        </div>
      )}

      {reposLoading && <Skeleton className="h-48 w-full" />}

      {repos && (
        <Card>
          <CardHeader>
            <CardTitle>Repositories</CardTitle>
          </CardHeader>
          <CardContent>
            <DataTable
              columns={repoColumns}
              data={repos}
              keyFn={row => row.id}
              emptyMessage="No repositories indexed yet"
            />
          </CardContent>
        </Card>
      )}

      {depsLoading && <Skeleton className="h-32 w-full" />}

      {deps && (
        <Card>
          <CardHeader>
            <CardTitle>Dependencies</CardTitle>
          </CardHeader>
          <CardContent>
            <DataTable
              columns={depColumns}
              data={deps}
              keyFn={row => row.id}
              emptyMessage="No dependencies recorded"
            />
          </CardContent>
        </Card>
      )}
    </div>
  )
}

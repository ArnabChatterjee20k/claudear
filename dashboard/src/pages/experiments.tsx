import useSWR from 'swr'
import { fetchExperiments, type PromptExperiment } from '../lib/api'
import { PageHeader } from '../components/layout/page-header'
import { Card, CardContent, CardHeader, CardTitle, CardDescription } from '../components/ui/card'
import { Skeleton } from '../components/ui/skeleton'
import { EmptyState } from '../components/shared/empty-state'

export default function ExperimentsPage() {
  const { data, error, isLoading } = useSWR<PromptExperiment[]>(
    'experiments',
    fetchExperiments,
    { refreshInterval: 30000 },
  )

  const groups: Record<string, PromptExperiment[]> = {}
  if (data) {
    for (const exp of data) {
      if (!groups[exp.experiment_name]) {
        groups[exp.experiment_name] = []
      }
      groups[exp.experiment_name].push(exp)
    }
  }

  const groupNames = Object.keys(groups).sort()

  return (
    <div className="space-y-6">
      <PageHeader title="Experiments" description="Prompt A/B testing results" />

      {error && (
        <div className="text-destructive text-sm">Failed to load experiments.</div>
      )}

      {isLoading && (
        <div className="space-y-4">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-48 w-full" />
          ))}
        </div>
      )}

      {data && groupNames.length === 0 && (
        <EmptyState message="No experiments recorded yet" />
      )}

      {groupNames.map(name => {
        const variants = groups[name]

        return (
          <Card key={name}>
            <CardHeader>
              <CardTitle>{name}</CardTitle>
              <CardDescription>
                {variants.length} variant{variants.length !== 1 ? 's' : ''}
              </CardDescription>
            </CardHeader>
            <CardContent>
              <div className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b">
                      <th className="text-left py-2 font-medium">Variant</th>
                      <th className="text-right py-2 font-medium">Success</th>
                      <th className="text-right py-2 font-medium">Failure</th>
                      <th className="text-right py-2 font-medium">Success Rate</th>
                      <th className="text-right py-2 font-medium">Avg Merge Time</th>
                      <th className="text-right py-2 font-medium">Avg Review Score</th>
                    </tr>
                  </thead>
                  <tbody>
                    {variants.map(v => {
                      const total = v.success_count + v.failure_count
                      const rate = total > 0 ? (v.success_count / total) * 100 : 0

                      return (
                        <tr key={v.id} className="border-b last:border-0">
                          <td className="py-2 font-medium">{v.variant}</td>
                          <td className="py-2 text-right text-green-600">
                            {v.success_count}
                          </td>
                          <td className="py-2 text-right text-red-600">
                            {v.failure_count}
                          </td>
                          <td className="py-2 text-right font-medium">
                            {rate.toFixed(1)}%
                          </td>
                          <td className="py-2 text-right">
                            {v.avg_time_to_merge != null
                              ? `${v.avg_time_to_merge.toFixed(1)}m`
                              : '--'}
                          </td>
                          <td className="py-2 text-right">
                            {v.avg_review_score != null
                              ? v.avg_review_score.toFixed(2)
                              : '--'}
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
              </div>
            </CardContent>
          </Card>
        )
      })}
    </div>
  )
}

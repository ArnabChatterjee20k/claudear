import useSWR from 'swr'
import { getHealth, type Health } from '../../lib/api'
import { Server, Loader2 } from 'lucide-react'

export function HealthBar() {
  const { data: health, error, isValidating } = useSWR<Health>('health', getHealth, {
    refreshInterval: 15000,
  })

  if (!health && !error) return null

  const unreachable = !!error

  return (
    <header className="border-b px-6 py-3 flex items-center justify-end gap-4 text-sm">
      <div className="flex items-center gap-2">
        <Server className="h-4 w-4" />
        {unreachable ? (
          <span className="text-red-500">Unreachable</span>
        ) : (
          <span className={health!.status === 'ok' ? 'text-green-500' : 'text-yellow-500'}>
            {health!.status === 'ok' ? 'Healthy' : 'Degraded'}
          </span>
        )}
      </div>
      {!unreachable && health && (
        <>
          <span className="text-muted-foreground">v{health.version}</span>
          <span className="text-muted-foreground">Uptime: {formatUptime(health.uptime_secs)}</span>
        </>
      )}
      {isValidating && (
        <Loader2 className="h-3 w-3 animate-spin text-muted-foreground" />
      )}
    </header>
  )
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`
  return `${Math.floor(seconds / 86400)}d ${Math.floor((seconds % 86400) / 3600)}h`
}

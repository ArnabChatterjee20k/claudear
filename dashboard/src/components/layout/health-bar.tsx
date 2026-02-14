import useSWR from 'swr'
import { getHealth, type Health } from '../../lib/api'
import { Server, Database } from 'lucide-react'

export function HealthBar() {
  const { data: health } = useSWR<Health>('health', getHealth, {
    refreshInterval: 30000,
  })

  if (!health) return null

  return (
    <header className="border-b px-6 py-3 flex items-center justify-end gap-4 text-sm">
      <div className="flex items-center gap-2">
        <Server className="h-4 w-4" />
        <span className={health.status === 'ok' ? 'text-green-500' : 'text-yellow-500'}>
          {health.status === 'ok' ? 'Healthy' : 'Degraded'}
        </span>
      </div>
      <div className="flex items-center gap-2">
        <Database className="h-4 w-4" />
        <span className={health.database.status === 'ok' ? 'text-green-500' : 'text-red-500'}>
          {health.database.status === 'ok' ? 'Connected' : 'Error'}
        </span>
      </div>
      <span className="text-muted-foreground">v{health.version}</span>
      <span className="text-muted-foreground">Uptime: {formatUptime(health.uptime_secs)}</span>
    </header>
  )
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`
  return `${Math.floor(seconds / 86400)}d ${Math.floor((seconds % 86400) / 3600)}h`
}

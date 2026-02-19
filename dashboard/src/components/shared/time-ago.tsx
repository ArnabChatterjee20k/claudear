import { formatDate } from '../../lib/formatters'

export function getTimeAgo(date: Date): string {
  const seconds = Math.floor((new Date().getTime() - date.getTime()) / 1000)
  if (seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86400)}d ago`
}

export function TimeAgo({ date }: { date: string | Date }) {
  const d = typeof date === 'string' ? new Date(date) : date
  return (
    <span className="text-sm text-muted-foreground" title={formatDate(d)}>
      {getTimeAgo(d)}
    </span>
  )
}

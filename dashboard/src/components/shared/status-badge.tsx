const statusColors: Record<string, string> = {
  pending: 'bg-yellow-100 text-yellow-800',
  success: 'bg-green-100 text-green-800',
  merged: 'bg-purple-100 text-purple-800',
  closed: 'bg-orange-100 text-orange-800',
  failed: 'bg-red-100 text-red-800',
  cannot_fix: 'bg-gray-100 text-gray-800',
  awaiting_release: 'bg-blue-100 text-blue-800',
  monitoring: 'bg-cyan-100 text-cyan-800',
  resolved: 'bg-green-100 text-green-800',
  regressed: 'bg-red-100 text-red-800',
  open: 'bg-blue-100 text-blue-800',
}

export function StatusBadge({ status }: { status: string }) {
  const color = statusColors[status] || 'bg-gray-100 text-gray-800'
  return (
    <span className={`px-2 py-1 rounded text-xs font-medium ${color}`}>
      {status.replace(/_/g, ' ')}
    </span>
  )
}

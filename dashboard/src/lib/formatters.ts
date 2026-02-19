export function formatNumber(n: number): string {
  if (n < 1000) return n.toString()
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`
  return `${(n / 1_000_000).toFixed(1)}M`
}

/**
 * Parse a date string from the API as UTC.
 *
 * SQLite stores dates via `datetime('now')` which produces UTC strings like
 * "2026-02-19 04:28:09" without a timezone marker. JavaScript's `new Date()`
 * treats such strings ambiguously (often as local time), so we append "Z" to
 * force UTC interpretation before converting to the user's local time.
 */
export function parseUTCDate(date: string): Date {
  // Already has timezone info (Z or +/- offset) — parse as-is
  if (/Z|[+-]\d{2}:\d{2}$/.test(date)) return new Date(date)
  // Replace space separator with T and append Z for ISO 8601 UTC
  return new Date(date.replace(' ', 'T') + 'Z')
}

export function formatDate(date: string | Date): string {
  const d = typeof date === 'string' ? parseUTCDate(date) : date
  return d.toLocaleString(undefined, {
    month: 'short',
    day: 'numeric',
    year: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  })
}

export function formatMins(mins: number | null): string {
  if (mins == null) return '--'
  if (mins < 60) return `${mins.toFixed(0)}m`
  if (mins < 1440) return `${(mins / 60).toFixed(1)}h`
  return `${(mins / 1440).toFixed(1)}d`
}

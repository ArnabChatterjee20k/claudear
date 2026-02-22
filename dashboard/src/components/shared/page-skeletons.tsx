import { cn } from '../../lib/utils'
import { PageHeader } from '../layout/page-header'
import { Skeleton } from '../ui/skeleton'

type GridSkeletonProps = {
  count: number
  className: string
  itemClassName?: string
}

export function GridSkeleton({
  count,
  className,
  itemClassName = 'h-28',
}: GridSkeletonProps) {
  return (
    <div className={cn('grid gap-4', className)}>
      {Array.from({ length: count }).map((_, i) => (
        <Skeleton key={i} className={itemClassName} />
      ))}
    </div>
  )
}

export function StatsGridSkeleton({
  count,
  className = 'md:grid-cols-2 lg:grid-cols-4',
  itemClassName = 'h-28',
}: {
  count: number
  className?: string
  itemClassName?: string
}) {
  return <GridSkeleton count={count} className={className} itemClassName={itemClassName} />
}

export function TableRowsSkeleton({
  rows = 5,
  rowClassName = 'h-10 w-full',
  className = 'space-y-2',
}: {
  rows?: number
  rowClassName?: string
  className?: string
}) {
  return (
    <div className={className}>
      {Array.from({ length: rows }).map((_, i) => (
        <Skeleton key={i} className={rowClassName} />
      ))}
    </div>
  )
}

export function CardStackSkeleton({
  count = 3,
  itemClassName = 'h-48 w-full',
  className = 'space-y-4',
}: {
  count?: number
  itemClassName?: string
  className?: string
}) {
  return (
    <div className={className}>
      {Array.from({ length: count }).map((_, i) => (
        <Skeleton key={i} className={itemClassName} />
      ))}
    </div>
  )
}

export function BlockSkeleton({ className = 'h-64 w-full' }: { className?: string }) {
  return <Skeleton className={className} />
}

export function OverviewPageSkeleton() {
  return (
    <div className="space-y-6">
      <PageHeader title="Overview" description="Monitor automated issue fixing" />
      <StatsGridSkeleton count={5} className="md:grid-cols-2 lg:grid-cols-5" />
      <StatsGridSkeleton count={6} className="md:grid-cols-2 lg:grid-cols-6" itemClassName="h-24" />
      <GridSkeleton count={2} className="md:grid-cols-2" itemClassName="h-72" />
      <BlockSkeleton className="h-80 w-full" />
    </div>
  )
}

export function UsersTableSkeleton({ rows = 5 }: { rows?: number }) {
  return (
    <div className="border rounded-lg overflow-hidden">
      <table className="w-full text-sm">
        <thead className="bg-muted/50">
          <tr>
            <th className="text-left p-3 font-medium">Name</th>
            <th className="text-left p-3 font-medium">Email</th>
            <th className="text-left p-3 font-medium">Role</th>
            <th className="text-left p-3 font-medium">Created</th>
            <th className="text-right p-3 font-medium">Actions</th>
          </tr>
        </thead>
        <tbody>
          {Array.from({ length: rows }).map((_, i) => (
            <tr key={i} className="border-t">
              <td className="p-3"><Skeleton className="h-4 w-28" /></td>
              <td className="p-3"><Skeleton className="h-4 w-40" /></td>
              <td className="p-3"><Skeleton className="h-5 w-16 rounded-full" /></td>
              <td className="p-3"><Skeleton className="h-4 w-24" /></td>
              <td className="p-3">
                <div className="flex justify-end gap-1">
                  <Skeleton className="h-7 w-7 rounded" />
                  <Skeleton className="h-7 w-7 rounded" />
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  )
}

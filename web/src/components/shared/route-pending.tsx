import { Skeleton } from '@/components/ui/skeleton'

export function RoutePending() {
  return (
    <div className="flex flex-col gap-4">
      <Skeleton className="h-8 w-48 rounded" />
      <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
        <Skeleton className="h-32 rounded-md" />
        <Skeleton className="h-32 rounded-md" />
      </div>
      <Skeleton className="h-64 rounded-md" />
    </div>
  )
}

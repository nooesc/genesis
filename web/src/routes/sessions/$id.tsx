import { createFileRoute } from '@tanstack/react-router'
import { RoutePending } from '@/components/shared/route-pending'

export const Route = createFileRoute('/sessions/\$id')({
  pendingComponent: RoutePending,
})

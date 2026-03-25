import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'
import { RoutePending } from '@/components/shared/route-pending'

const searchSchema = z.object({
  focusNodeId: z.string().optional(),
  focusMode: z.enum(['focus', 'select']).optional(),
})

export const Route = createFileRoute('/monitor')({
  validateSearch: searchSchema,
  pendingComponent: RoutePending,
})

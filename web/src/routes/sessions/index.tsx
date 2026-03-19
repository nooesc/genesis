import { createFileRoute } from '@tanstack/react-router'
import { z } from 'zod'

const searchSchema = z.object({
  search: z.string().optional(),
})

export const Route = createFileRoute('/sessions/')({
  validateSearch: searchSchema,
})

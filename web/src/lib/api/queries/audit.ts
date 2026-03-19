import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { AuditEntry } from '../types'

interface AuditParams {
  limit?: number
  offset?: number
}

interface AuditQueryOptions {
  refetchInterval?: number
}

export function useAuditLog(params?: AuditParams, options?: AuditQueryOptions) {
  const searchParams = new URLSearchParams()
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  if (params?.offset !== undefined) searchParams.set('offset', String(params.offset))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['audit', params],
    queryFn: () => api.get<AuditEntry[]>(`/audit${qs ? `?${qs}` : ''}`),
    refetchInterval: options?.refetchInterval,
  })
}

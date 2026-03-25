import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { AuditEntry } from '../types'

interface AuditParams {
  limit?: number
  offset?: number
}

interface AuditQueryOptions {
  refetchInterval?: number
  enabled?: boolean
}

interface AuditResponse {
  entries: AuditEntry[]
  count: number
}

export function useAuditLog(params?: AuditParams, options?: AuditQueryOptions) {
  const searchParams = new URLSearchParams()
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  if (params?.offset !== undefined) searchParams.set('offset', String(params.offset))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['audit', params],
    queryFn: async () => {
      const res = await api.get<AuditResponse>(`/audit${qs ? `?${qs}` : ''}`)
      return res.entries
    },
    refetchInterval: options?.refetchInterval,
    enabled: options?.enabled ?? true,
  })
}

/** Fetch just the latest audit entry for the system bar ticker */
export function useLatestAuditEvent() {
  return useQuery({
    queryKey: ['audit', 'latest'],
    queryFn: async () => {
      const res = await api.get<AuditResponse>('/audit?limit=1')
      return res.entries[0] ?? null
    },
    refetchInterval: 10_000,
  })
}

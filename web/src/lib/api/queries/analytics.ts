import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { InsightsData, UsageStats } from '../types'

interface InsightsOptions {
  refetchInterval?: number
}

export function useInsights(days: number = 30, options?: InsightsOptions) {
  return useQuery({
    queryKey: ['insights', days],
    queryFn: () => api.get<InsightsData>(`/insights?days=${days}`),
    refetchInterval: options?.refetchInterval ?? 60_000,
  })
}

export function useUsage() {
  return useQuery({
    queryKey: ['usage'],
    queryFn: () => api.get<UsageStats>('/usage'),
    refetchInterval: 60_000,
  })
}

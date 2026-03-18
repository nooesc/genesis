import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { InsightsData, UsageStats } from '../types'

export function useInsights(days: number = 30) {
  return useQuery({
    queryKey: ['insights', days],
    queryFn: () => api.get<InsightsData>(`/insights?days=${days}`),
  })
}

export function useUsage() {
  return useQuery({
    queryKey: ['usage'],
    queryFn: () => api.get<UsageStats>('/usage'),
  })
}

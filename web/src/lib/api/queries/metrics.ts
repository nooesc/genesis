import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { MetricsJsonResponse } from '../types'

export function useMetricsJson() {
  return useQuery({
    queryKey: ['metrics'],
    queryFn: () => api.get<MetricsJsonResponse>('/metrics/json'),
    refetchInterval: 10_000,
  })
}

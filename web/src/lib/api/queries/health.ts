import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { HealthResponse, McpStatusResponse } from '../types'

export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: () => api.get<HealthResponse>('/health'),
    refetchInterval: 5_000,
  })
}

export function useMcpStatus() {
  return useQuery({
    queryKey: ['health', 'mcp'],
    queryFn: () => api.get<McpStatusResponse>('/health/mcp'),
    refetchInterval: 5_000,
  })
}

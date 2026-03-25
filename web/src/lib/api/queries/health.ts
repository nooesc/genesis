import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { HealthResponse, McpStatusResponse, CacheStatsResponse, WebhookStatusResponse } from '../types'

interface QueryToggleOptions {
  enabled?: boolean
}

export function useHealth(options?: QueryToggleOptions) {
  return useQuery({
    queryKey: ['health'],
    queryFn: () => api.get<HealthResponse>('/health'),
    refetchInterval: 5_000,
    enabled: options?.enabled ?? true,
  })
}

export function useMcpStatus() {
  return useQuery({
    queryKey: ['health', 'mcp'],
    queryFn: () => api.get<McpStatusResponse>('/health/mcp'),
    refetchInterval: 30_000,
  })
}

export function useCacheStats() {
  return useQuery({
    queryKey: ['cache', 'stats'],
    queryFn: () => api.get<CacheStatsResponse>('/cache/stats'),
    refetchInterval: 30_000,
  })
}

export function useWebhookStatus() {
  return useQuery({
    queryKey: ['webhooks', 'status'],
    queryFn: () => api.get<WebhookStatusResponse>('/webhooks/status'),
    refetchInterval: 30_000,
  })
}

import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { HealthResponse, McpStatusResponse, CacheStatsResponse, WebhookStatusResponse } from '../types'

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

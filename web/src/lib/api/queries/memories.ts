import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Memory } from '../types'

interface MemoriesParams {
  limit?: number
}

export function useMemories(params?: MemoriesParams) {
  const searchParams = new URLSearchParams()
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['memories', params],
    queryFn: () => api.get<Memory[]>(`/memories${qs ? `?${qs}` : ''}`),
  })
}

export function useSearchMemories(query: string) {
  return useQuery({
    queryKey: ['memories', 'search', query],
    queryFn: () => {
      const qs = new URLSearchParams({ q: query }).toString()
      return api.get<Memory[]>(`/memories/search?${qs}`)
    },
    enabled: Boolean(query),
  })
}

import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Memory } from '../types'

interface MemoriesParams {
  limit?: number
}

interface MemoriesResponse {
  memories: Memory[]
  count: number
}

interface SearchResponse {
  memories: Memory[]
  count: number
  mode: string
}

export function useMemories(params?: MemoriesParams) {
  const searchParams = new URLSearchParams()
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['memories', params],
    queryFn: async () => {
      const res = await api.get<MemoriesResponse>(`/memories${qs ? `?${qs}` : ''}`)
      return res.memories
    },
    refetchInterval: 60_000,
  })
}

export function useSearchMemories(query: string) {
  return useQuery({
    queryKey: ['memories', 'search', query],
    queryFn: async () => {
      const qs = new URLSearchParams({ q: query }).toString()
      const res = await api.get<SearchResponse>(`/memories/search?${qs}`)
      return res.memories
    },
    enabled: Boolean(query),
  })
}

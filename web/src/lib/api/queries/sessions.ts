import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { SessionSummary, SessionsResponse, StoredMessage } from '../types'

interface SessionsParams {
  search?: string
  limit?: number
}

export function useSessions(params?: SessionsParams) {
  const searchParams = new URLSearchParams()
  if (params?.search) searchParams.set('search', params.search)
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['sessions', params],
    queryFn: async () => {
      const res = await api.get<SessionsResponse>(`/sessions${qs ? `?${qs}` : ''}`)
      return res.sessions as SessionSummary[]
    },
    refetchInterval: 30_000,
  })
}

/** Returns sessions + total count from API (not truncated by limit) */
export function useSessionsWithCount(params?: SessionsParams) {
  const searchParams = new URLSearchParams()
  if (params?.search) searchParams.set('search', params.search)
  if (params?.limit !== undefined) searchParams.set('limit', String(params.limit))
  const qs = searchParams.toString()

  return useQuery({
    queryKey: ['sessions-with-count', params],
    queryFn: () => api.get<SessionsResponse>(`/sessions${qs ? `?${qs}` : ''}`),
    refetchInterval: 30_000,
  })
}

export function useSession(id: string) {
  return useQuery({
    queryKey: ['sessions', id],
    queryFn: () => api.get<SessionSummary>(`/sessions/${id}`),
    enabled: Boolean(id),
  })
}

interface MessagesResponse {
  session_id: string
  messages: StoredMessage[]
  count: number
}

export function useMessages(sessionId: string) {
  return useQuery({
    queryKey: ['sessions', sessionId, 'messages'],
    queryFn: async () => {
      const res = await api.get<MessagesResponse>(`/sessions/${sessionId}/messages`)
      return res.messages
    },
    enabled: Boolean(sessionId),
  })
}

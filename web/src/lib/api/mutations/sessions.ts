import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../client'
import type { SessionSummary } from '../types'

export function useDeleteSession() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/sessions/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
    },
  })
}

export function useForkSession() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => api.post<SessionSummary>(`/sessions/${id}/fork`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
    },
  })
}

export function useUpdateTitle() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, title }: { id: string; title: string }) =>
      api.patch<void>(`/sessions/${id}/title`, { title }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
    },
  })
}

export function useAddTag() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, tag }: { id: string; tag: string }) =>
      api.post<void>(`/sessions/${id}/tags`, { tag }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
    },
  })
}

export function useRemoveTag() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, tag }: { id: string; tag: string }) =>
      api.delete<void>(`/sessions/${id}/tags/${encodeURIComponent(tag)}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['sessions'] })
    },
  })
}

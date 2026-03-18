import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../client'

export function useDeleteMemory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/memories/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['memories'] })
    },
  })
}

export function useEmbedMemory() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (content: string) =>
      api.post<{ id: string }>('/memories/embed', { content }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['memories'] })
    },
  })
}

export function useEmbedAll() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (contents: string[]) =>
      api.post<{ embedded: number }>('/memories/embed/batch', { contents }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['memories'] })
    },
  })
}

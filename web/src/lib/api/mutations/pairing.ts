import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../client'

export function useApprovePairing() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (params: { platform: string; code: string }) =>
      api.post<void>('/pairing/approve', params),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['pairing'] })
    },
  })
}

export function useRevokePairing() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (params: { platform: string; user_id: string }) =>
      api.post<void>('/pairing/revoke', params),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['pairing'] })
    },
  })
}

export function useClearPending() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (platform?: string) =>
      api.post<void>('/pairing/clear-pending', platform ? { platform } : {}),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['pairing'] })
    },
  })
}

import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { ApprovedListResponse, PendingListResponse } from '../types'

export function useApprovedUsers() {
  return useQuery({
    queryKey: ['pairing', 'approved'],
    queryFn: () => api.get<ApprovedListResponse>('/pairing/approved'),
    refetchInterval: 30_000,
  })
}

export function usePendingPairings() {
  return useQuery({
    queryKey: ['pairing', 'pending'],
    queryFn: () => api.get<PendingListResponse>('/pairing/pending'),
    refetchInterval: 10_000,
  })
}

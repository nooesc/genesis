import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../client'
import type { Schedule } from '../types'

interface CreateScheduleInput {
  id?: string
  cron_expression: string
  destination: string
  prompt: string
}

export function useCreateSchedule() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (input: CreateScheduleInput) => api.post<Schedule>('/schedules', input),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['schedules'] })
    },
  })
}

export function useDeleteSchedule() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (id: string) => api.delete<void>(`/schedules/${id}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['schedules'] })
    },
  })
}

export function useToggleSchedule() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      api.patch<void>(`/schedules/${id}/enabled`, { enabled }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['schedules'] })
    },
  })
}

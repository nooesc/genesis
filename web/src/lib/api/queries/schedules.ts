import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Schedule } from '../types'

interface SchedulesResponse {
  schedules: Schedule[]
  count: number
}

interface SchedulesQueryOptions {
  enabled?: boolean
}

export function useSchedules(options?: SchedulesQueryOptions) {
  return useQuery({
    queryKey: ['schedules'],
    queryFn: async () => {
      const res = await api.get<SchedulesResponse>('/schedules')
      return res.schedules
    },
    refetchInterval: 30_000,
    enabled: options?.enabled ?? true,
  })
}

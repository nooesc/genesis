import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Schedule } from '../types'

interface SchedulesResponse {
  schedules: Schedule[]
  count: number
}

export function useSchedules() {
  return useQuery({
    queryKey: ['schedules'],
    queryFn: async () => {
      const res = await api.get<SchedulesResponse>('/schedules')
      return res.schedules
    },
    refetchInterval: 30_000,
  })
}

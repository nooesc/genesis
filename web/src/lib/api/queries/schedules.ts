import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Schedule } from '../types'

export function useSchedules() {
  return useQuery({
    queryKey: ['schedules'],
    queryFn: () => api.get<Schedule[]>('/schedules'),
  })
}

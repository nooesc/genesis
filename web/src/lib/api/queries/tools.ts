import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { ToolInfo } from '../types'

export function useTools() {
  return useQuery({
    queryKey: ['tools'],
    queryFn: () => api.get<ToolInfo[]>('/tools'),
  })
}

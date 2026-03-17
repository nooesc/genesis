import { useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../client'
import type { Skill } from '../types'

interface CreateSkillInput {
  name: string
  description: string
  content: string
  tags?: string[]
}

export function useCreateSkill() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (input: CreateSkillInput) => api.post<Skill>('/skills', input),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['skills'] })
    },
  })
}

export function useDeleteSkill() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (name: string) => api.delete<void>(`/skills/${encodeURIComponent(name)}`),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['skills'] })
    },
  })
}

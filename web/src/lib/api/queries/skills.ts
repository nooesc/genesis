import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Skill, SkillUsageStats, SkillUsageRecord } from '../types'

export function useSkills() {
  return useQuery({
    queryKey: ['skills'],
    queryFn: () => api.get<Skill[]>('/skills'),
  })
}

export function useSkill(name: string) {
  return useQuery({
    queryKey: ['skills', name],
    queryFn: () => api.get<Skill>(`/skills/${encodeURIComponent(name)}`),
    enabled: Boolean(name),
  })
}

export function useSkillUsage(name: string) {
  return useQuery({
    queryKey: ['skills', name, 'usage'],
    queryFn: async () => {
      const [stats, recent] = await Promise.all([
        api.get<SkillUsageStats>(`/skills/${encodeURIComponent(name)}/usage`),
        api.get<SkillUsageRecord[]>(`/skills/${encodeURIComponent(name)}/usage/recent`),
      ])
      return { stats, recent }
    },
    enabled: Boolean(name),
  })
}

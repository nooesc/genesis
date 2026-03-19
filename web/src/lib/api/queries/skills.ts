import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { Skill, SkillUsageStats, SkillUsageRecord } from '../types'

interface SkillsResponse {
  skills: Skill[]
  count: number
}

interface SkillUsageRecentResponse {
  skill_name: string
  usages: SkillUsageRecord[]
  count: number
}

export function useSkills() {
  return useQuery({
    queryKey: ['skills'],
    queryFn: async () => {
      const res = await api.get<SkillsResponse>('/skills')
      return res.skills
    },
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
      const [stats, recentRes] = await Promise.all([
        api.get<SkillUsageStats>(`/skills/${encodeURIComponent(name)}/usage`),
        api.get<SkillUsageRecentResponse>(`/skills/${encodeURIComponent(name)}/usage/recent`),
      ])
      return { stats, recent: recentRes.usages }
    },
    enabled: Boolean(name),
  })
}

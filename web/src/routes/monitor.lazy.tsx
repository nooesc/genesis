import { createLazyFileRoute } from '@tanstack/react-router'
import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useAuditLog } from '@/lib/api/queries/audit'
import { useSchedules } from '@/lib/api/queries/schedules'
import { useSessions } from '@/lib/api/queries/sessions'
import { useSkills } from '@/lib/api/queries/skills'
import { useMemo } from 'react'
import { CommandMap } from '@/components/monitor/command-map'
import { Skeleton } from '@/components/ui/skeleton'
import { buildCommandMapModel } from '@/lib/command-map/selectors'
import type { CommandMapProjectionInput } from '@/lib/command-map/types'

export const Route = createLazyFileRoute('/monitor')({
  component: MonitorPage,
})

function MonitorPage() {
  const search = Route.useSearch()
  const { data: health, isLoading: healthLoading } = useHealth()
  const { data: insights, isLoading: insightsLoading } = useInsights(7, { refetchInterval: 60_000 })
  const { data: sessions, isLoading: sessionsLoading } = useSessions({ limit: 20 })
  const { data: schedules, isLoading: schedulesLoading } = useSchedules()
  const { data: skills, isLoading: skillsLoading } = useSkills()
  const { data: audit = [], isLoading: auditLoading } = useAuditLog({ limit: 24 }, { refetchInterval: 30_000 })

  const model = useMemo(() => {
    const input: CommandMapProjectionInput = {
      health: health ?? null,
      sessions: sessions ?? [],
      schedules: schedules ?? [],
      skills: skills ?? [],
      audit,
      insights: insights ?? null,
    }

    return buildCommandMapModel(input)
  }, [audit, health, insights, schedules, sessions, skills])

  const isLoading = healthLoading || insightsLoading || sessionsLoading || schedulesLoading || skillsLoading || auditLoading

  if (isLoading) {
    return (
      <div className="flex flex-col gap-4">
        <Skeleton className="h-10 w-64 rounded" />
        <Skeleton className="h-[420px] w-full rounded-xl" />
      </div>
    )
  }

  return <CommandMap model={model} focusNodeId={search.focusNodeId ?? null} focusMode={search.focusMode ?? 'select'} />
}

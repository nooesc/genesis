import { createFileRoute } from '@tanstack/react-router'
import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useSessions } from '@/lib/api/queries/sessions'
import { AgentCanvas } from '@/components/monitor/agent-canvas'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { getPlatformColor } from '@/lib/platforms'

export const Route = createFileRoute('/monitor')({
  component: MonitorPage,
})

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const parts: string[] = []
  if (d > 0) parts.push(`${d}d`)
  if (h > 0) parts.push(`${h}h`)
  parts.push(`${m}m`)
  return parts.join(' ')
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`
  return String(n)
}

function MonitorPage() {
  const { data: health, isLoading: healthLoading } = useHealth()
  const { data: insights, isLoading: insightsLoading } = useInsights(7, { refetchInterval: 60_000 })
  const { data: sessions, isLoading: sessionsLoading } = useSessions({ limit: 20 })

  const isHealthy = health?.status === 'ok' || health?.status === 'healthy'
  const totalTokens7d = insights
    ? insights.tokens_per_day.reduce((sum, [, inp, out]) => sum + inp + out, 0)
    : 0

  const isLoading = healthLoading || insightsLoading || sessionsLoading

  return (
    <div className="flex h-full gap-4">
      {/* Canvas area */}
      <div className="flex flex-1 items-center justify-center">
        {isLoading ? (
          <Skeleton className="h-[400px] w-[500px] rounded-xl" />
        ) : (
          <AgentCanvas
            sessions={sessions ?? []}
            isHealthy={isHealthy}
            totalTools={health?.total_tools ?? 0}
            uptimeSeconds={health?.uptime_seconds ?? 0}
            toolUsage={insights?.tool_usage ?? []}
          />
        )}
      </div>

      {/* Stats sidebar */}
      <div className="flex w-64 shrink-0 flex-col gap-3">
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              System Status
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 pb-3">
            <StatRow label="Status" value={isHealthy ? 'Healthy' : health ? 'Degraded' : '...'} color={isHealthy ? 'text-emerald-400' : 'text-red-400'} />
            <StatRow label="Uptime" value={health ? formatUptime(health.uptime_seconds) : '—'} />
            <StatRow label="Version" value={health?.version ?? '—'} />
            <StatRow label="Model" value={health?.model ?? '—'} />
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Activity (7d)
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 pb-3">
            <StatRow label="Sessions" value={String(insights?.sessions_count ?? '—')} />
            <StatRow label="Tokens" value={insightsLoading ? '—' : formatTokens(totalTokens7d)} />
            <StatRow label="Avg In" value={insights ? formatTokens(insights.avg_input_tokens) : '—'} />
            <StatRow label="Avg Out" value={insights ? formatTokens(insights.avg_output_tokens) : '—'} />
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Infrastructure
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 pb-3">
            <StatRow label="Tools" value={String(health?.total_tools ?? '—')} />
            <StatRow label="MCP Servers" value={String(health?.mcp_servers ?? '—')} />
            <StatRow label="Schedules" value={String(health?.active_schedules ?? '—')} />
          </CardContent>
        </Card>

        {/* Platform legend */}
        <Card>
          <CardHeader className="pb-1">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Platforms
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-3">
            <div className="flex flex-wrap gap-2">
              {(insights?.platform_breakdown ?? []).map(([platform, count]) => (
                <div key={platform} className="flex items-center gap-1.5">
                  <div
                    className="h-2 w-2 rounded-full"
                    style={{ backgroundColor: getPlatformColor(platform) }}
                  />
                  <span className="font-mono text-[10px] text-muted-foreground">
                    {platform} ({count})
                  </span>
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

function StatRow({ label, value, color }: { label: string; value: string; color?: string }) {
  return (
    <div className="flex items-center justify-between">
      <span className="font-mono text-[10px] text-muted-foreground/50">{label}</span>
      <span className={`font-mono text-[11px] tabular-nums ${color ?? 'text-foreground/80'}`}>
        {value}
      </span>
    </div>
  )
}


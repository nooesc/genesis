import { createFileRoute } from '@tanstack/react-router'
import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useSessions } from '@/lib/api/queries/sessions'
import { HealthGauge } from '@/components/dashboard/health-gauge'
import { Sparkline } from '@/components/dashboard/sparkline'
import { TokenChart } from '@/components/dashboard/token-chart'
import { PlatformBreakdown } from '@/components/dashboard/platform-breakdown'
import { RecentSessions } from '@/components/dashboard/recent-sessions'
import { ActivityFeed } from '@/components/dashboard/activity-feed'
import { ToolHeatmap } from '@/components/dashboard/tool-heatmap'
import { Skeleton } from '@/components/ui/skeleton'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'

export const Route = createFileRoute('/')({
  component: DashboardPage,
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

function DashboardPage() {
  const { data: health, isLoading: healthLoading } = useHealth()
  const { data: insights, isLoading: insightsLoading } = useInsights(7)
  const { data: sessions, isLoading: sessionsLoading } = useSessions({ limit: 10 })

  // tokens_per_day is [date, input, output][] — extract totals for sparkline
  const tokensPerDayValues = insights
    ? [...insights.tokens_per_day]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([, inp, out]) => inp + out)
    : []

  // sessions_per_day is [date, count][]
  const sessionsPerDayValues = insights
    ? [...insights.sessions_per_day]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([, count]) => count)
    : []

  const totalTokens7d = tokensPerDayValues.reduce((sum, v) => sum + v, 0)

  // platform_breakdown is already [platform, count][]
  const platformEntries: [string, number][] = insights
    ? insights.platform_breakdown
    : []

  const isHealthy = health?.status === 'ok' || health?.status === 'healthy'

  // Uptime as fraction of 30 days (reasonable gauge max)
  const uptimeFraction = health
    ? Math.min(health.uptime_seconds / (30 * 86400), 1)
    : 0

  return (
    <div className="flex flex-col gap-4">
      {/* Row 1: System gauges + KPI metrics */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Health gauge */}
        <Card className="lg:col-span-2">
          <CardContent className="flex items-center justify-center py-4">
            {healthLoading ? (
              <Skeleton className="h-[100px] w-[100px] rounded-full" />
            ) : (
              <HealthGauge
                value={isHealthy ? uptimeFraction : 0.15}
                label={isHealthy ? 'OK' : health ? '!!' : '?'}
                status={isHealthy ? 'success' : health ? 'error' : 'warning'}
                sublabel={health ? `Up ${formatUptime(health.uptime_seconds)}` : 'unreachable'}
              />
            )}
          </CardContent>
        </Card>

        {/* Metric cards with sparklines */}
        <Card className="lg:col-span-3">
          <CardContent className="flex items-center justify-between gap-3 p-4">
            <div className="flex flex-col">
              <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/60">
                Tokens (7d)
              </span>
              <span className="font-mono text-xl font-bold tabular-nums text-foreground">
                {insightsLoading ? '—' : formatTokens(totalTokens7d)}
              </span>
              <span className="font-mono text-[10px] text-muted-foreground/40">
                input + output
              </span>
            </div>
            {tokensPerDayValues.length >= 2 && (
              <Sparkline data={tokensPerDayValues} width={80} height={28} color="#0891b2" />
            )}
          </CardContent>
        </Card>

        <Card className="lg:col-span-3">
          <CardContent className="flex items-center justify-between gap-3 p-4">
            <div className="flex flex-col">
              <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/60">
                Sessions (7d)
              </span>
              <span className="font-mono text-xl font-bold tabular-nums text-foreground">
                {insightsLoading ? '—' : insights?.sessions_count ?? 0}
              </span>
              <span className="font-mono text-[10px] text-muted-foreground/40">
                v{health?.version ?? '—'}
              </span>
            </div>
            {sessionsPerDayValues.length >= 2 && (
              <Sparkline data={sessionsPerDayValues} width={80} height={28} color="#a855f7" />
            )}
          </CardContent>
        </Card>

        <Card className="lg:col-span-2">
          <CardContent className="flex flex-col gap-1 p-4">
            <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/60">
              Tools
            </span>
            <span className="font-mono text-xl font-bold tabular-nums text-foreground">
              {health?.total_tools ?? 0}
            </span>
            <span className="font-mono text-[10px] text-muted-foreground/40">
              {health?.mcp_servers ?? 0} MCP
            </span>
          </CardContent>
        </Card>

        <Card className="lg:col-span-2">
          <CardContent className="flex flex-col gap-1 p-4">
            <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/60">
              Schedules
            </span>
            <span className="font-mono text-xl font-bold tabular-nums text-foreground">
              {health?.active_schedules ?? 0}
            </span>
            <span className="font-mono text-[10px] text-muted-foreground/40">
              active
            </span>
          </CardContent>
        </Card>
      </div>

      {/* Row 2: Token chart + Tool heatmap */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <Card className="lg:col-span-2">
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Token Throughput (7d)
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-4">
            {insightsLoading ? (
              <Skeleton className="h-[200px] w-full rounded" />
            ) : (
              <TokenChart tokensPerDay={insights?.tokens_per_day ?? []} />
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Tool Usage Heatmap
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-4">
            {insightsLoading ? (
              <Skeleton className="h-[200px] w-full rounded" />
            ) : (
              <ToolHeatmap toolUsage={insights?.tool_usage ?? []} />
            )}
          </CardContent>
        </Card>
      </div>

      {/* Row 3: Activity feed + Platform breakdown + Recent sessions */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Live Activity
            </CardTitle>
          </CardHeader>
          <CardContent className="max-h-[300px] overflow-auto pb-4">
            <ActivityFeed />
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Platform Breakdown
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-4">
            {insightsLoading ? (
              <div className="flex flex-col gap-3">
                {Array.from({ length: 3 }).map((_, i) => (
                  <Skeleton key={i} className="h-6 w-full rounded" />
                ))}
              </div>
            ) : (
              <PlatformBreakdown data={platformEntries} />
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground/60">
              Recent Sessions
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-4">
            {sessionsLoading ? (
              <div className="flex flex-col gap-2">
                {Array.from({ length: 5 }).map((_, i) => (
                  <Skeleton key={i} className="h-8 w-full rounded" />
                ))}
              </div>
            ) : (
              <RecentSessions sessions={sessions ?? []} />
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

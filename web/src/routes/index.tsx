import { createFileRoute } from '@tanstack/react-router'
import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useSessions } from '@/lib/api/queries/sessions'
import { KpiCard } from '@/components/dashboard/kpi-card'
import { TokenChart } from '@/components/dashboard/token-chart'
import { PlatformBreakdown } from '@/components/dashboard/platform-breakdown'
import { RecentSessions } from '@/components/dashboard/recent-sessions'
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

  // tokens_per_day is [date, input, output][] — sum input + output
  const totalTokens7d = insights
    ? insights.tokens_per_day.reduce((sum, [, inp, out]) => sum + inp + out, 0)
    : 0

  // platform_breakdown is already [platform, count][]
  const platformEntries: [string, number][] = insights
    ? insights.platform_breakdown
    : []

  const isHealthy = health?.status === 'ok' || health?.status === 'healthy'

  return (
    <div className="flex flex-col gap-6">
      {/* KPI row */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {healthLoading ? (
          Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-[80px] rounded-xl" />
          ))
        ) : (
          <>
            <KpiCard
              label="Status"
              value={isHealthy ? 'Healthy' : 'Degraded'}
              subtitle={health ? `Up ${formatUptime(health.uptime_seconds)}` : undefined}
              status={isHealthy ? 'success' : 'error'}
            />
            <KpiCard
              label="Total Sessions"
              value={health?.total_sessions ?? 0}
              subtitle={`v${health?.version ?? '—'}`}
            />
            <KpiCard
              label="Tokens (7d)"
              value={insightsLoading ? '—' : formatTokens(totalTokens7d)}
              subtitle="input + output"
            />
            <KpiCard
              label="Active Tools"
              value={health?.total_tools ?? 0}
              subtitle={`${health?.mcp_servers ?? 0} MCP server${(health?.mcp_servers ?? 0) !== 1 ? 's' : ''}`}
            />
          </>
        )}
      </div>

      {/* Charts row */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
        <Card className="lg:col-span-2">
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
              Tokens / Day (7d)
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
            <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
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
      </div>

      {/* Recent sessions */}
      <Card>
        <CardHeader className="pb-2">
          <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
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
  )
}

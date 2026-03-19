import { createFileRoute } from '@tanstack/react-router'
import { useHealth, useCacheStats, useWebhookStatus } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useSessions } from '@/lib/api/queries/sessions'
import { HealthGauge } from '@/components/dashboard/health-gauge'
import { Sparkline } from '@/components/dashboard/sparkline'
import { TokenChart } from '@/components/dashboard/token-chart'
import { PlatformBreakdown } from '@/components/dashboard/platform-breakdown'
import { RecentSessions } from '@/components/dashboard/recent-sessions'
import { ActivityFeed } from '@/components/dashboard/activity-feed'
import { ToolHeatmap } from '@/components/dashboard/tool-heatmap'
import { SectionHeader } from '@/components/shared/section-header'
import { Skeleton } from '@/components/ui/skeleton'
import { formatUptime, formatTokens, isHealthyStatus } from '@/lib/utils'

export const Route = createFileRoute('/')({
  component: DashboardPage,
})

function DashboardPage() {
  const { data: health, isLoading: healthLoading } = useHealth()
  const { data: insights, isLoading: insightsLoading } = useInsights(7)
  const { data: sessions, isLoading: sessionsLoading } = useSessions({ limit: 10 })
  const { data: cacheStats } = useCacheStats()
  const { data: webhookStatus } = useWebhookStatus()

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

  const isHealthy = isHealthyStatus(health?.status)

  // Uptime as fraction of 30 days (reasonable gauge max)
  const uptimeFraction = health
    ? Math.min(health.uptime_seconds / (30 * 86400), 1)
    : 0

  return (
    <div className="flex flex-col gap-5">
      {/* Row 1: System gauges + KPI metrics */}
      <div className="card-stagger grid grid-cols-1 gap-3 lg:grid-cols-12">
        {/* Health gauge */}
        <div className="flex items-center justify-center rounded-md border border-border/20 bg-card/20 py-4 lg:col-span-2">
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
        </div>

        {/* Metric cards with sparklines */}
        <div className="flex items-center justify-between gap-3 rounded-md border border-border/20 bg-card/20 px-4 py-3 lg:col-span-3">
          <div className="flex flex-col">
            <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
              Tokens (7d)
            </span>
            <span className="mt-0.5 font-mono text-lg font-bold tabular-nums text-foreground/90">
              {insightsLoading ? '—' : formatTokens(totalTokens7d)}
            </span>
            <span className="font-mono text-[9px] text-muted-foreground/30">
              input + output
            </span>
          </div>
          {tokensPerDayValues.length >= 2 && (
            <Sparkline data={tokensPerDayValues} width={80} height={28} color="#0891b2" />
          )}
        </div>

        <div className="flex items-center justify-between gap-3 rounded-md border border-border/20 bg-card/20 px-4 py-3 lg:col-span-3">
          <div className="flex flex-col">
            <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
              Sessions (7d)
            </span>
            <span className="mt-0.5 font-mono text-lg font-bold tabular-nums text-foreground/90">
              {insightsLoading ? '—' : insights?.sessions_count ?? 0}
            </span>
            <span className="font-mono text-[9px] text-muted-foreground/30">
              v{health?.version ?? '—'}
            </span>
          </div>
          {sessionsPerDayValues.length >= 2 && (
            <Sparkline data={sessionsPerDayValues} width={80} height={28} color="#a855f7" />
          )}
        </div>

        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-4 py-3 lg:col-span-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Tools
          </span>
          <span className="font-mono text-lg font-bold tabular-nums text-foreground/90">
            {health?.total_tools ?? 0}
          </span>
          <span className="font-mono text-[9px] text-muted-foreground/30">
            {health?.mcp_servers ?? 0} MCP
          </span>
        </div>

        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-4 py-3 lg:col-span-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Schedules
          </span>
          <span className="font-mono text-lg font-bold tabular-nums text-foreground/90">
            {health?.active_schedules ?? 0}
          </span>
          <span className="font-mono text-[9px] text-muted-foreground/30">
            active
          </span>
        </div>
      </div>

      {/* Row 2: Token chart + Tool heatmap */}
      <SectionHeader title="Throughput" />
      <div className="card-stagger grid grid-cols-1 gap-3 lg:grid-cols-3">
        <div className="rounded-md border border-border/20 bg-card/20 p-3 lg:col-span-2">
          <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
            Token Throughput (7d)
          </div>
          {insightsLoading ? (
            <Skeleton className="h-[200px] w-full rounded" />
          ) : (
            <TokenChart tokensPerDay={insights?.tokens_per_day ?? []} />
          )}
        </div>

        <div className="rounded-md border border-border/20 bg-card/20 p-3">
          <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
            Tool Usage Heatmap
          </div>
          {insightsLoading ? (
            <Skeleton className="h-[200px] w-full rounded" />
          ) : (
            <ToolHeatmap toolUsage={insights?.tool_usage ?? []} />
          )}
        </div>
      </div>

      {/* Row 3: Operations readouts */}
      <SectionHeader title="Operations" />
      <div className="card-stagger grid grid-cols-2 gap-3 lg:grid-cols-4">
        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-3 py-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Cache
          </span>
          <span className="font-mono text-sm font-bold tabular-nums text-foreground/90">
            {cacheStats?.entries ?? 0}
          </span>
          <span className="font-mono text-[8px] text-muted-foreground/30">
            {cacheStats?.total_hits ?? 0} hits · {cacheStats?.enabled ? 'on' : 'off'}
          </span>
        </div>
        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-3 py-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Webhooks
          </span>
          <span className="font-mono text-sm font-bold tabular-nums text-foreground/90">
            {webhookStatus?.delivered ?? 0}
          </span>
          <span className="font-mono text-[8px] text-muted-foreground/30">
            delivered · {webhookStatus?.retried ?? 0} retried
          </span>
        </div>
        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-3 py-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Failures
          </span>
          <span className={`font-mono text-sm font-bold tabular-nums ${(webhookStatus?.failed ?? 0) > 0 ? 'text-destructive/80' : 'text-foreground/90'}`}>
            {webhookStatus?.failed ?? 0}
          </span>
          <span className="font-mono text-[8px] text-muted-foreground/30">
            {webhookStatus?.dead_letter_count ?? 0} dead letters
          </span>
        </div>
        <div className="flex flex-col gap-1 rounded-md border border-border/20 bg-card/20 px-3 py-2">
          <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
            Model
          </span>
          <span className="truncate font-mono text-sm font-bold text-foreground/90">
            {health?.model ? health.model.split('/').pop() ?? health.model : '—'}
          </span>
          <span className="font-mono text-[8px] text-muted-foreground/30">
            {health?.total_sessions ?? 0} total sessions
          </span>
        </div>
      </div>

      {/* Row 4: Activity feed + Platform breakdown + Recent sessions */}
      <SectionHeader title="Activity" />
      <div className="card-stagger grid grid-cols-1 gap-3 lg:grid-cols-3">
        <div className="rounded-md border border-border/20 bg-card/20 p-3">
          <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
            Live Activity
          </div>
          <div className="max-h-[280px] overflow-auto">
            <ActivityFeed />
          </div>
        </div>

        <div className="rounded-md border border-border/20 bg-card/20 p-3">
          <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
            Platform Breakdown
          </div>
          {insightsLoading ? (
            <div className="flex flex-col gap-3">
              {Array.from({ length: 3 }).map((_, i) => (
                <Skeleton key={i} className="h-6 w-full rounded" />
              ))}
            </div>
          ) : (
            <PlatformBreakdown data={platformEntries} />
          )}
        </div>

        <div className="rounded-md border border-border/20 bg-card/20 p-3">
          <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
            Recent Sessions
          </div>
          {sessionsLoading ? (
            <div className="flex flex-col gap-2">
              {Array.from({ length: 5 }).map((_, i) => (
                <Skeleton key={i} className="h-8 w-full rounded" />
              ))}
            </div>
          ) : (
            <RecentSessions sessions={sessions ?? []} />
          )}
        </div>
      </div>
    </div>
  )
}

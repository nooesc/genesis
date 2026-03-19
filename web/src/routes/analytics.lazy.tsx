import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'
import { useInsights, useUsage } from '@/lib/api/queries/analytics'
import { formatTokens } from '@/lib/utils'
import { TokenChart } from '@/components/dashboard/token-chart'
import { PlatformBreakdown } from '@/components/dashboard/platform-breakdown'
import { KpiCard } from '@/components/dashboard/kpi-card'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'

export const Route = createLazyFileRoute('/analytics')({
  component: AnalyticsPage,
})

type Period = 7 | 30 | 90

function formatDateLabel(dateStr: string): string {
  try {
    const d = new Date(dateStr)
    return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
  } catch {
    return dateStr
  }
}

function SessionsPerDayChart({ sessionsPerDay }: { sessionsPerDay: [string, number][] }) {
  const data = [...sessionsPerDay]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([date, sessions]) => ({
      date: formatDateLabel(date),
      sessions,
    }))

  if (data.length === 0) {
    return (
      <div className="flex h-[200px] items-center justify-center font-mono text-xs text-muted-foreground">
        No session data available
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={200}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" vertical={false} />
        <XAxis
          dataKey="date"
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
        />
        <YAxis
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
          allowDecimals={false}
          width={28}
        />
        <Tooltip
          cursor={{ fill: 'hsl(var(--accent))' }}
          contentStyle={{
            backgroundColor: 'hsl(var(--popover))',
            border: '1px solid hsl(var(--border))',
            borderRadius: '6px',
            fontFamily: 'var(--font-geist-mono, monospace)',
            fontSize: '11px',
            color: 'hsl(var(--popover-foreground))',
          }}
          labelStyle={{ color: 'hsl(var(--muted-foreground))' }}
          itemStyle={{ color: '#16a34a' }}
        />
        <Bar dataKey="sessions" name="Sessions" fill="#16a34a" radius={[2, 2, 0, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

function ToolUsageChart({ toolUsage }: { toolUsage: [string, number][] }) {
  const data = [...toolUsage]
    .sort(([, a], [, b]) => b - a)
    .slice(0, 20)
    .map(([tool, count]) => ({ tool, count }))

  if (data.length === 0) {
    return (
      <div className="flex h-[300px] items-center justify-center font-mono text-xs text-muted-foreground">
        No tool usage data
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={Math.max(200, data.length * 28)}>
      <BarChart
        data={data}
        layout="vertical"
        margin={{ top: 4, right: 16, left: 8, bottom: 4 }}
      >
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" horizontal={false} />
        <XAxis
          type="number"
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
          tickFormatter={(v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}k` : String(v))}
        />
        <YAxis
          type="category"
          dataKey="tool"
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
          width={140}
        />
        <Tooltip
          cursor={{ fill: 'hsl(var(--accent))' }}
          contentStyle={{
            backgroundColor: 'hsl(var(--popover))',
            border: '1px solid hsl(var(--border))',
            borderRadius: '6px',
            fontFamily: 'var(--font-geist-mono, monospace)',
            fontSize: '11px',
            color: 'hsl(var(--popover-foreground))',
          }}
          labelStyle={{ color: 'hsl(var(--muted-foreground))' }}
          itemStyle={{ color: '#0891b2' }}
        />
        <Bar dataKey="count" name="Calls" fill="#0891b2" radius={[0, 2, 2, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

const PERIODS: { label: string; value: Period }[] = [
  { label: '7d', value: 7 },
  { label: '30d', value: 30 },
  { label: '90d', value: 90 },
]

function AnalyticsPage() {
  const [period, setPeriod] = React.useState<Period>(30)
  const { data: insights, isLoading: insightsLoading } = useInsights(period)
  const { data: usage, isLoading: usageLoading } = useUsage()

  const platformEntries: [string, number][] = insights
    ? insights.platform_breakdown
    : []

  const avgTokensPerSession =
    insights && insights.sessions_count > 0
      ? Math.round(
          (insights.total_input_tokens + insights.total_output_tokens) / insights.sessions_count,
        )
      : 0

  return (
    <div className="flex flex-col gap-6">
      {/* Header + period selector */}
      <div className="flex items-center justify-between gap-3">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Analytics
        </h1>
        <div className="flex items-center gap-1 rounded-md border border-border bg-muted p-1">
          {PERIODS.map(({ label, value }) => (
            <button
              key={value}
              onClick={() => setPeriod(value)}
              className={[
                'rounded px-3 py-1 font-mono text-xs transition-colors',
                period === value
                  ? 'bg-background text-foreground shadow-sm'
                  : 'text-muted-foreground hover:text-foreground',
              ].join(' ')}
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {/* KPI summary */}
      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {insightsLoading || usageLoading ? (
          Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-[80px] rounded-xl" />
          ))
        ) : (
          <>
            <KpiCard
              label="Sessions"
              value={insights?.sessions_count ?? 0}
              subtitle={`last ${period}d`}
            />
            <KpiCard
              label="Input Tokens"
              value={formatTokens(insights?.total_input_tokens ?? 0)}
              subtitle={`last ${period}d`}
            />
            <KpiCard
              label="Output Tokens"
              value={formatTokens(insights?.total_output_tokens ?? 0)}
              subtitle={`last ${period}d`}
            />
            <KpiCard
              label="Avg Tokens / Session"
              value={formatTokens(avgTokensPerSession)}
              subtitle={`all time: ${formatTokens((usage?.total_input_tokens ?? 0) + (usage?.total_output_tokens ?? 0))}`}
            />
          </>
        )}
      </div>

      {/* Token chart + sessions per day */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
              Tokens / Day
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
              Sessions / Day
            </CardTitle>
          </CardHeader>
          <CardContent className="pb-4">
            {insightsLoading ? (
              <Skeleton className="h-[200px] w-full rounded" />
            ) : (
              <SessionsPerDayChart sessionsPerDay={insights?.sessions_per_day ?? []} />
            )}
          </CardContent>
        </Card>
      </div>

      {/* Platform breakdown + tool usage */}
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-3">
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

        <Card className="lg:col-span-2">
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
              Top 20 Tools
            </CardTitle>
          </CardHeader>
          <CardContent className="overflow-x-auto pb-4">
            {insightsLoading ? (
              <Skeleton className="h-[300px] w-full rounded" />
            ) : (
              <ToolUsageChart toolUsage={insights?.tool_usage ?? []} />
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

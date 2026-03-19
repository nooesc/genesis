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
import { Sparkline } from '@/components/dashboard/sparkline'
import { Skeleton } from '@/components/ui/skeleton'
import { getPlatformColor } from '@/lib/platforms'
import { SectionHeader } from '@/components/shared/section-header'
import { PageHeader } from '@/components/shared/page-header'
import { BarChart3Icon } from 'lucide-react'

export const Route = createLazyFileRoute('/analytics')({
  component: AnalyticsPage,
})

type Period = 7 | 30 | 90

const PERIODS: { label: string; value: Period }[] = [
  { label: '7d', value: 7 },
  { label: '30d', value: 30 },
  { label: '90d', value: 90 },
]

const CHART_TOOLTIP_STYLE = {
  backgroundColor: 'hsl(var(--popover))',
  border: '1px solid hsl(var(--border))',
  borderRadius: '4px',
  fontFamily: 'var(--font-mono)',
  fontSize: '10px',
  color: 'hsl(var(--popover-foreground))',
  padding: '4px 8px',
} as const

const AXIS_TICK = {
  fontFamily: 'var(--font-mono)',
  fontSize: 9,
} as const

function formatDateLabel(dateStr: string): string {
  try {
    const d = new Date(dateStr)
    return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
  } catch {
    return dateStr
  }
}

/** Metric cell used in the KPI strip */
function Metric({
  label,
  value,
  sub,
  sparkData,
  sparkColor,
}: {
  label: string
  value: string | number
  sub?: string
  sparkData?: number[]
  sparkColor?: string
}) {
  return (
    <div className="flex items-center gap-3 rounded-md border border-border/20 bg-card/20 px-3 py-2">
      <div className="min-w-0 flex-1">
        <div className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
          {label}
        </div>
        <div className="mt-0.5 font-mono text-sm font-bold tabular-nums text-foreground/90">
          {value}
        </div>
        {sub && (
          <div className="font-mono text-[8px] tabular-nums text-muted-foreground/30">
            {sub}
          </div>
        )}
      </div>
      {sparkData && sparkData.length >= 2 && (
        <Sparkline data={sparkData} width={64} height={20} color={sparkColor ?? '#0891b2'} />
      )}
    </div>
  )
}

function TokensPerDayChart({ tokensPerDay }: { tokensPerDay: [string, number, number][] }) {
  const data = React.useMemo(
    () =>
      [...tokensPerDay]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([date, inp, out]) => ({
          date: formatDateLabel(date),
          input: inp,
          output: out,
        })),
    [tokensPerDay],
  )

  if (data.length === 0) {
    return (
      <div className="flex h-[180px] items-center justify-center font-mono text-[10px] text-muted-foreground/40">
        No token data
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={180}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" vertical={false} />
        <XAxis dataKey="date" tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" />
        <YAxis tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" tickFormatter={(v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}k` : String(v))} width={32} />
        <Tooltip cursor={{ fill: 'hsl(var(--accent))' }} contentStyle={CHART_TOOLTIP_STYLE} labelStyle={{ color: 'hsl(var(--muted-foreground))' }} />
        <Bar dataKey="input" name="Input" fill="#0891b2" stackId="tokens" radius={[0, 0, 0, 0]} />
        <Bar dataKey="output" name="Output" fill="#a855f7" stackId="tokens" radius={[2, 2, 0, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

function SessionsPerDayChart({ sessionsPerDay }: { sessionsPerDay: [string, number][] }) {
  const data = React.useMemo(
    () =>
      [...sessionsPerDay]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([date, sessions]) => ({
          date: formatDateLabel(date),
          sessions,
        })),
    [sessionsPerDay],
  )

  if (data.length === 0) {
    return (
      <div className="flex h-[180px] items-center justify-center font-mono text-[10px] text-muted-foreground/40">
        No session data
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={180}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" vertical={false} />
        <XAxis dataKey="date" tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" />
        <YAxis tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" allowDecimals={false} width={24} />
        <Tooltip cursor={{ fill: 'hsl(var(--accent))' }} contentStyle={CHART_TOOLTIP_STYLE} labelStyle={{ color: 'hsl(var(--muted-foreground))' }} />
        <Bar dataKey="sessions" name="Sessions" fill="#22c55e" radius={[2, 2, 0, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

function ToolUsageChart({ toolUsage }: { toolUsage: [string, number][] }) {
  const data = React.useMemo(
    () =>
      [...toolUsage]
        .sort(([, a], [, b]) => b - a)
        .slice(0, 15)
        .map(([tool, count]) => ({ tool, count })),
    [toolUsage],
  )

  if (data.length === 0) {
    return (
      <div className="flex h-[200px] items-center justify-center font-mono text-[10px] text-muted-foreground/40">
        No tool usage data
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={Math.max(160, data.length * 24)}>
      <BarChart data={data} layout="vertical" margin={{ top: 4, right: 12, left: 4, bottom: 4 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" horizontal={false} />
        <XAxis type="number" tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" tickFormatter={(v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}k` : String(v))} />
        <YAxis type="category" dataKey="tool" tick={AXIS_TICK} tickLine={false} axisLine={false} stroke="hsl(var(--muted-foreground))" width={120} />
        <Tooltip cursor={{ fill: 'hsl(var(--accent))' }} contentStyle={CHART_TOOLTIP_STYLE} labelStyle={{ color: 'hsl(var(--muted-foreground))' }} />
        <Bar dataKey="count" name="Calls" fill="#0891b2" radius={[0, 2, 2, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

function PlatformBars({ data }: { data: [string, number][] }) {
  const total = data.reduce((sum, [, count]) => sum + count, 0)
  if (data.length === 0 || total === 0) {
    return (
      <div className="flex h-full items-center justify-center font-mono text-[10px] text-muted-foreground/40">
        No platform data
      </div>
    )
  }

  const sorted = [...data].sort(([, a], [, b]) => b - a)

  return (
    <div className="flex flex-col gap-2.5">
      {sorted.map(([platform, count]) => {
        const pct = Math.round((count / total) * 100)
        const color = getPlatformColor(platform)
        return (
          <div key={platform} className="flex flex-col gap-1">
            <div className="flex items-center justify-between">
              <span className="font-mono text-[10px] capitalize text-foreground/70">{platform}</span>
              <span className="font-mono text-[9px] tabular-nums text-muted-foreground/40">
                {count} <span className="text-muted-foreground/25">({pct}%)</span>
              </span>
            </div>
            <div className="h-1 w-full overflow-hidden rounded-full bg-border/20">
              <div
                className="h-full rounded-full transition-all duration-500"
                style={{ width: `${pct}%`, backgroundColor: color }}
              />
            </div>
          </div>
        )
      })}
    </div>
  )
}

// SectionHeader imported from shared component

function AnalyticsPage() {
  const [period, setPeriod] = React.useState<Period>(30)
  const { data: insights, isLoading: insightsLoading } = useInsights(period)
  const { data: usage, isLoading: usageLoading } = useUsage()

  const tokensPerDayValues = React.useMemo(
    () =>
      insights
        ? [...insights.tokens_per_day]
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([, inp, out]) => inp + out)
        : [],
    [insights],
  )

  const sessionsPerDayValues = React.useMemo(
    () =>
      insights
        ? [...insights.sessions_per_day]
            .sort(([a], [b]) => a.localeCompare(b))
            .map(([, count]) => count)
        : [],
    [insights],
  )

  const avgTokensPerSession =
    insights && insights.sessions_count > 0
      ? Math.round(
          (insights.total_input_tokens + insights.total_output_tokens) / insights.sessions_count,
        )
      : 0

  const allTimeTokens = (usage?.total_input_tokens ?? 0) + (usage?.total_output_tokens ?? 0)

  return (
    <div className="flex flex-col gap-5">
      <PageHeader title="Analytics" icon={BarChart3Icon}>
        <div className="flex items-center gap-1">
          {PERIODS.map(({ label, value }) => (
            <button
              key={value}
              onClick={() => setPeriod(value)}
              className={`rounded px-2.5 py-0.5 font-mono text-[9px] uppercase tracking-wider transition-colors ${
                period === value
                  ? 'bg-primary/10 text-primary'
                  : 'text-muted-foreground/40 hover:text-muted-foreground'
              }`}
            >
              {label}
            </button>
          ))}
        </div>
      </PageHeader>

      {/* KPI metrics strip */}
      {insightsLoading || usageLoading ? (
        <div className="grid grid-cols-2 gap-2 lg:grid-cols-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-16 rounded-md" />
          ))}
        </div>
      ) : (
        <div className="card-stagger grid grid-cols-2 gap-2 lg:grid-cols-4">
          <Metric
            label="Sessions"
            value={insights?.sessions_count ?? 0}
            sub={`last ${period}d`}
            sparkData={sessionsPerDayValues}
            sparkColor="#22c55e"
          />
          <Metric
            label="Input Tokens"
            value={formatTokens(insights?.total_input_tokens ?? 0)}
            sub={`avg ${formatTokens(insights?.avg_input_tokens ?? 0)}/session`}
          />
          <Metric
            label="Output Tokens"
            value={formatTokens(insights?.total_output_tokens ?? 0)}
            sub={`avg ${formatTokens(insights?.avg_output_tokens ?? 0)}/session`}
          />
          <Metric
            label="Avg / Session"
            value={formatTokens(avgTokensPerSession)}
            sub={`all-time: ${formatTokens(allTimeTokens)}`}
            sparkData={tokensPerDayValues}
            sparkColor="#0891b2"
          />
        </div>
      )}

      {/* Token + Sessions charts */}
      <SectionHeader title="Activity" />
      {insightsLoading ? (
        <div className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          <Skeleton className="h-[210px] rounded-md" />
          <Skeleton className="h-[210px] rounded-md" />
        </div>
      ) : (
        <div className="card-stagger grid grid-cols-1 gap-3 lg:grid-cols-2">
          <div className="rounded-md border border-border/20 bg-card/20 p-3">
            <div className="mb-2 flex items-center justify-between">
              <span className="font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
                Tokens / Day
              </span>
              <div className="flex items-center gap-3">
                <span className="flex items-center gap-1 font-mono text-[8px] text-muted-foreground/30">
                  <span className="inline-block h-1.5 w-3 rounded-sm bg-[#0891b2]" /> input
                </span>
                <span className="flex items-center gap-1 font-mono text-[8px] text-muted-foreground/30">
                  <span className="inline-block h-1.5 w-3 rounded-sm bg-[#a855f7]" /> output
                </span>
              </div>
            </div>
            <TokensPerDayChart tokensPerDay={insights?.tokens_per_day ?? []} />
          </div>

          <div className="rounded-md border border-border/20 bg-card/20 p-3">
            <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
              Sessions / Day
            </div>
            <SessionsPerDayChart sessionsPerDay={insights?.sessions_per_day ?? []} />
          </div>
        </div>
      )}

      {/* Platform + Tool usage */}
      <SectionHeader title="Breakdown" />
      {insightsLoading ? (
        <div className="grid grid-cols-1 gap-3 lg:grid-cols-3">
          <Skeleton className="h-[200px] rounded-md" />
          <Skeleton className="col-span-2 h-[200px] rounded-md" />
        </div>
      ) : (
        <div className="card-stagger grid grid-cols-1 gap-3 lg:grid-cols-3">
          <div className="rounded-md border border-border/20 bg-card/20 p-3">
            <div className="mb-3 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
              Platforms
            </div>
            <PlatformBars data={insights?.platform_breakdown ?? []} />
          </div>

          <div className="rounded-md border border-border/20 bg-card/20 p-3 lg:col-span-2">
            <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
              Top 15 Tools
            </div>
            <ToolUsageChart toolUsage={insights?.tool_usage ?? []} />
          </div>
        </div>
      )}
    </div>
  )
}

import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useEffect, useState } from 'react'

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  if (d > 0) return `${d}d ${h}h`
  if (h > 0) return `${h}h ${m}m`
  return `${m}m`
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`
  return String(n)
}

function SystemClock() {
  const [time, setTime] = useState(() => new Date())

  useEffect(() => {
    const id = setInterval(() => setTime(new Date()), 1000)
    return () => clearInterval(id)
  }, [])

  return (
    <span className="font-mono text-[11px] tabular-nums text-muted-foreground">
      {time.toLocaleTimeString('en-US', { hour12: false })}
    </span>
  )
}

export function SystemBar() {
  const { data: health, isError } = useHealth()
  const { data: insights } = useInsights(7)

  const totalTokens7d = insights
    ? Object.values(insights.tokens_per_day).reduce((sum, v) => sum + v, 0)
    : 0

  const isHealthy = health?.status === 'ok' || health?.status === 'healthy'

  return (
    <header className="system-bar flex h-9 items-center justify-between border-b border-border/50 bg-[#0c0c0c] px-4">
      {/* Left: Branding + Connection */}
      <div className="flex items-center gap-3">
        <div className="flex items-center gap-2">
          <div
            className={`h-[6px] w-[6px] rounded-full ${
              isError
                ? 'bg-red-500 shadow-[0_0_6px_rgba(239,68,68,0.6)]'
                : isHealthy
                  ? 'bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.5)] animate-[pulse-glow_3s_ease-in-out_infinite]'
                  : 'bg-amber-400 shadow-[0_0_6px_rgba(251,191,36,0.5)]'
            }`}
          />
          <span className="font-mono text-[11px] font-semibold tracking-wider text-primary">
            GENESIS
          </span>
        </div>
        {health && (
          <span className="font-mono text-[10px] text-muted-foreground/60">
            v{health.version}
          </span>
        )}
      </div>

      {/* Center: System Vitals */}
      <div className="flex items-center gap-1">
        {health && (
          <>
            <StatusChip
              label="UP"
              value={formatUptime(health.uptime_seconds)}
            />
            <Divider />
            <StatusChip
              label="SESSIONS"
              value={String(health.total_sessions)}
            />
            <Divider />
            <StatusChip
              label="TOOLS"
              value={String(health.total_tools)}
            />
            <Divider />
            <StatusChip
              label="7D TOK"
              value={formatTokens(totalTokens7d)}
            />
            {health.active_schedules > 0 && (
              <>
                <Divider />
                <StatusChip
                  label="SCHED"
                  value={String(health.active_schedules)}
                />
              </>
            )}
          </>
        )}
      </div>

      {/* Right: Clock */}
      <div className="flex items-center gap-3">
        <span className="font-mono text-[10px] text-muted-foreground/40">
          {isError ? 'OFFLINE' : health ? 'ONLINE' : '...'}
        </span>
        <SystemClock />
      </div>
    </header>
  )
}

function StatusChip({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center gap-1.5 px-2">
      <span className="font-mono text-[9px] uppercase tracking-wider text-muted-foreground/50">
        {label}
      </span>
      <span className="font-mono text-[11px] tabular-nums text-foreground/80">
        {value}
      </span>
    </div>
  )
}

function Divider() {
  return <div className="h-3 w-px bg-border/40" />
}

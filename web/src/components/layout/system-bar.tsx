import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useLatestAuditEvent } from '@/lib/api/queries/audit'
import { formatUptime, formatTokens, isHealthyStatus, formatRelativeTime } from '@/lib/utils'
import { useEffect, useMemo, useState } from 'react'

function SystemClock() {
  const [time, setTime] = useState(() => new Date())

  useEffect(() => {
    const id = setInterval(() => setTime(new Date()), 1000)
    return () => clearInterval(id)
  }, [])

  return (
    <span className="font-mono text-[11px] tabular-nums text-foreground/60">
      {time.toLocaleTimeString('en-US', { hour12: false })}
    </span>
  )
}

export function SystemBar() {
  const { data: health, isError } = useHealth()
  const { data: insights } = useInsights(7)
  const { data: latestEvent } = useLatestAuditEvent()

  const totalTokens7d = useMemo(
    () => insights ? insights.tokens_per_day.reduce((sum, [, inp, out]) => sum + inp + out, 0) : 0,
    [insights],
  )

  const isHealthy = isHealthyStatus(health?.status)

  return (
    <header className="system-bar relative flex h-9 items-center justify-between border-b border-border/30 bg-[#0a0a0a] px-4">
      {/* Subtle bottom glow line */}
      <div className="pointer-events-none absolute inset-x-0 bottom-0 h-px bg-gradient-to-r from-transparent via-primary/20 to-transparent" />

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
          <span className="bg-gradient-to-r from-primary to-primary/60 bg-clip-text font-mono text-[11px] font-bold tracking-[0.2em] text-transparent">
            GENESIS
          </span>
        </div>
        {health && (
          <span className="font-mono text-[9px] tabular-nums text-muted-foreground/40">
            v{health.version}
          </span>
        )}
      </div>

      {/* Center: System Vitals */}
      <div className="flex items-center gap-1">
        {health && (
          <>
            <StatusChip label="UP" value={formatUptime(health.uptime_seconds)} />
            <Divider />
            <StatusChip label="SESSIONS" value={String(health.total_sessions)} />
            <Divider />
            <StatusChip label="TOOLS" value={String(health.total_tools)} />
            <Divider />
            <StatusChip label="7D TOK" value={formatTokens(totalTokens7d)} />
            {health.active_schedules > 0 && (
              <>
                <Divider />
                <StatusChip label="SCHED" value={String(health.active_schedules)} />
              </>
            )}
          </>
        )}
      </div>

      {/* Right: Latest event + status + clock */}
      <div className="flex items-center gap-3">
        {latestEvent && (
          <span className="max-w-[180px] truncate font-mono text-[8px] text-muted-foreground/20" title={`${latestEvent.action} — ${formatRelativeTime(latestEvent.created_at)}`}>
            {latestEvent.action}
          </span>
        )}
        <span className={`font-mono text-[9px] font-medium tracking-wider ${
          isError ? 'text-red-400' : health ? 'text-emerald-400/60' : 'text-muted-foreground/30'
        }`}>
          {isError ? 'OFFLINE' : health ? 'ONLINE' : '···'}
        </span>
        <Divider />
        <SystemClock />
      </div>
    </header>
  )
}

function StatusChip({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center gap-1.5 px-2">
      <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
        {label}
      </span>
      <span className="font-mono text-[10px] tabular-nums text-foreground/70">
        {value}
      </span>
    </div>
  )
}

function Divider() {
  return <div className="h-3 w-px bg-border/20" />
}

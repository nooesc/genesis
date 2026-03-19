import { createLazyFileRoute } from '@tanstack/react-router'
import { useHealth, useMcpStatus } from '@/lib/api/queries/health'
import { useSchedules } from '@/lib/api/queries/schedules'
import { useInsights } from '@/lib/api/queries/analytics'
import { Skeleton } from '@/components/ui/skeleton'
import { formatUptime, isHealthyStatus } from '@/lib/utils'
import { getPlatformColor } from '@/lib/platforms'
import {
  Bot, Server, Clock, Cpu, Activity,
  MessageSquare, Globe,
} from 'lucide-react'

export const Route = createLazyFileRoute('/agents')({
  component: AgentsPage,
})

function TopoNode({
  label,
  subtitle,
  status,
  icon: Icon,
  accent,
}: {
  label: string
  subtitle?: string
  status?: 'online' | 'offline' | 'idle'
  icon: React.ComponentType<{ className?: string }>
  accent?: string
}) {
  return (
    <div className="rounded-lg border border-border/40 bg-card/40 px-3 py-2 transition-colors hover:border-border/60">
      <div className="flex items-center gap-2">
        <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded bg-muted/30">
          <Icon className="h-3 w-3 text-muted-foreground" />
        </div>
        <div className="min-w-0">
          <div className="truncate font-mono text-[10px] font-medium text-foreground/80">{label}</div>
          {subtitle && (
            <div className="truncate font-mono text-[8px] text-muted-foreground/40">{subtitle}</div>
          )}
        </div>
      </div>
      {status && (
        <div className="mt-1.5 flex items-center gap-1.5">
          <div
            className={`h-1.5 w-1.5 rounded-full ${
              status === 'online' ? 'bg-emerald-400 shadow-[0_0_4px_rgba(52,211,153,0.5)]'
              : status === 'offline' ? 'bg-red-400'
              : 'bg-muted-foreground/40'
            }`}
            style={accent && status === 'online' ? { backgroundColor: accent, boxShadow: `0 0 4px ${accent}60` } : undefined}
          />
          <span className={`font-mono text-[8px] ${
            status === 'online' ? 'text-emerald-400'
            : status === 'offline' ? 'text-red-400'
            : 'text-muted-foreground/40'
          }`}
            style={accent && status === 'online' ? { color: accent } : undefined}
          >
            {status === 'online' ? 'Online' : status === 'offline' ? 'Offline' : 'Idle'}
          </span>
        </div>
      )}
    </div>
  )
}

function AgentsPage() {
  const { data: health, isLoading: healthLoading } = useHealth()
  const { data: mcpStatus, isLoading: mcpLoading } = useMcpStatus()
  const { data: schedules } = useSchedules()
  const { data: insights } = useInsights(7)

  const isHealthy = isHealthyStatus(health?.status)
  const isLoading = healthLoading || mcpLoading

  const platforms = insights?.platform_breakdown ?? []
  const activeSchedules = (schedules ?? []).filter(s => s.enabled)
  const mcpServers = mcpStatus?.servers ?? []

  if (isLoading) {
    return (
      <div className="flex flex-col gap-4">
        <Skeleton className="h-8 w-48 rounded" />
        <Skeleton className="h-[500px] w-full rounded-lg" />
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-4">
      <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
        Agent Topology
      </h1>

      <div className="relative overflow-hidden rounded-lg border border-border/20 bg-[#0b0b0b] p-6">
        {/* Dot grid background */}
        <div className="pointer-events-none absolute inset-0 opacity-[0.03]" style={{
          backgroundImage: 'radial-gradient(circle, var(--foreground) 1px, transparent 1px)',
          backgroundSize: '24px 24px',
        }} />

        {/* 3-column topology: Services | Eve | Platforms */}
        <div className="relative z-10 flex items-start justify-center gap-6">
          {/* Left: MCP Services */}
          <div className="flex w-48 flex-col gap-2 pt-12">
            {mcpServers.length > 0 && (
              <>
                <span className="mb-1 font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                  MCP Services
                </span>
                {mcpServers.map((server) => (
                  <div key={server.name} className="flex items-center gap-2">
                    <TopoNode
                      label={server.name}
                      status={server.connected ? 'online' : 'offline'}
                      icon={Server}
                    />
                    {/* Connector dash */}
                    <div className="h-px w-6 border-t border-dashed border-border/30" />
                  </div>
                ))}
              </>
            )}
          </div>

          {/* Center: Eve */}
          <div className="flex flex-col items-center gap-4">
            {/* Eve Card */}
            <div className="rounded-xl border-2 border-primary/30 bg-card/60 px-6 py-4 shadow-lg shadow-primary/5">
              <div className="flex items-center gap-3">
                <div className="flex h-10 w-10 items-center justify-center rounded-lg bg-primary/10">
                  <Bot className="h-5 w-5 text-primary" />
                </div>
                <div>
                  <div className="font-mono text-sm font-semibold text-foreground">Eve</div>
                  <div className="font-mono text-[9px] text-muted-foreground/50">
                    {health?.model ?? 'agent'} · v{health?.version ?? '—'}
                  </div>
                </div>
              </div>
              <div className="mt-3 flex items-center gap-3 border-t border-border/20 pt-3">
                <div className="flex items-center gap-1.5">
                  <div className={`h-2 w-2 rounded-full ${isHealthy ? 'bg-emerald-400 shadow-[0_0_6px_rgba(52,211,153,0.5)]' : 'bg-red-400'}`} />
                  <span className={`font-mono text-[10px] ${isHealthy ? 'text-emerald-400' : 'text-red-400'}`}>
                    {isHealthy ? 'Online' : 'Degraded'}
                  </span>
                </div>
                <span className="font-mono text-[9px] text-muted-foreground/30">
                  Up {formatUptime(health?.uptime_seconds ?? 0)}
                </span>
              </div>
              <div className="mt-2 flex gap-4">
                <MiniStat icon={Cpu} value={String(health?.total_tools ?? 0)} label="tools" />
                <MiniStat icon={Activity} value={String(health?.total_sessions ?? 0)} label="sessions" />
                <MiniStat icon={Clock} value={String(health?.active_schedules ?? 0)} label="schedules" />
              </div>
            </div>

            {/* Vertical connector to schedules */}
            {activeSchedules.length > 0 && (
              <div className="h-8 w-px border-l border-dashed border-border/30" />
            )}

            {/* Schedules (below Eve) */}
            {activeSchedules.length > 0 && (
              <div className="flex flex-col items-center gap-2">
                <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                  Active Schedules
                </span>
                <div className="flex flex-wrap justify-center gap-2">
                  {activeSchedules.slice(0, 4).map((schedule) => (
                    <TopoNode
                      key={schedule.id}
                      label={schedule.destination || schedule.id}
                      subtitle={schedule.cron_expression}
                      status="idle"
                      icon={Clock}
                    />
                  ))}
                  {activeSchedules.length > 4 && (
                    <div className="flex items-center px-2 font-mono text-[9px] text-muted-foreground/30">
                      +{activeSchedules.length - 4} more
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {/* Right: Platforms */}
          <div className="flex w-48 flex-col gap-2 pt-12">
            {platforms.length > 0 && (
              <>
                <span className="mb-1 font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                  Platforms
                </span>
                {platforms.map(([platform, count]) => (
                  <div key={platform} className="flex items-center gap-2">
                    {/* Connector dash */}
                    <div className="h-px w-6 border-t border-dashed border-border/30" />
                    <TopoNode
                      label={platform}
                      subtitle={`${count} sessions`}
                      status="online"
                      icon={platform === 'api' ? Globe : MessageSquare}
                      accent={getPlatformColor(platform)}
                    />
                  </div>
                ))}
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  )
}

function MiniStat({ icon: Icon, value, label }: { icon: React.ComponentType<{ className?: string }>; value: string; label: string }) {
  return (
    <div className="flex items-center gap-1.5">
      <Icon className="h-3 w-3 text-muted-foreground/30" />
      <span className="font-mono text-[10px] tabular-nums text-foreground/60">{value}</span>
      <span className="font-mono text-[8px] text-muted-foreground/30">{label}</span>
    </div>
  )
}

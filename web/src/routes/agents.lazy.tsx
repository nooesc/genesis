import { createLazyFileRoute } from '@tanstack/react-router'
import { useHealth, useMcpStatus } from '@/lib/api/queries/health'
import { useSchedules } from '@/lib/api/queries/schedules'
import { useInsights } from '@/lib/api/queries/analytics'
import { Skeleton } from '@/components/ui/skeleton'
import { formatUptime, isHealthyStatus } from '@/lib/utils'
import { getPlatformColor } from '@/lib/platforms'
import {
  Bot, Server, Webhook, Clock, Cpu, Activity,
  MessageSquare, Globe,
} from 'lucide-react'

export const Route = createLazyFileRoute('/agents')({
  component: AgentsPage,
})

// --- Topology Node Component ---
function TopoNode({
  label,
  subtitle,
  status,
  icon: Icon,
  accent,
  size = 'md',
}: {
  label: string
  subtitle?: string
  status?: 'online' | 'offline' | 'idle'
  icon: React.ComponentType<{ className?: string }>
  accent?: string
  size?: 'sm' | 'md' | 'lg'
}) {
  const sizeClasses = {
    sm: 'px-3 py-2',
    md: 'px-4 py-3',
    lg: 'px-5 py-4',
  }

  return (
    <div className={`rounded-lg border border-border/40 bg-card/40 ${sizeClasses[size]} transition-colors hover:border-border/60`}>
      <div className="flex items-center gap-2.5">
        <div className="flex h-7 w-7 items-center justify-center rounded-md bg-muted/30">
          <Icon className="h-3.5 w-3.5 text-muted-foreground" />
        </div>
        <div className="min-w-0">
          <div className="font-mono text-[11px] font-medium text-foreground/80">{label}</div>
          {subtitle && (
            <div className="truncate font-mono text-[9px] text-muted-foreground/40">{subtitle}</div>
          )}
        </div>
      </div>
      {status && (
        <div className="mt-2 flex items-center gap-1.5">
          <div
            className={`h-1.5 w-1.5 rounded-full ${
              status === 'online' ? 'bg-emerald-400 shadow-[0_0_4px_rgba(52,211,153,0.5)]'
              : status === 'offline' ? 'bg-red-400'
              : 'bg-muted-foreground/40'
            }`}
            style={accent && status === 'online' ? { backgroundColor: accent, boxShadow: `0 0 4px ${accent}60` } : undefined}
          />
          <span className={`font-mono text-[9px] ${
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

// --- SVG Connector (dashed line between two points) ---
function Connector({ x1, y1, x2, y2 }: { x1: number; y1: number; x2: number; y2: number }) {
  // Step connector (right-angle path like Railway)
  const midY = (y1 + y2) / 2
  return (
    <path
      d={`M ${x1} ${y1} L ${x1} ${midY} L ${x2} ${midY} L ${x2} ${y2}`}
      fill="none"
      stroke="var(--border)"
      strokeWidth={1}
      strokeDasharray="4 4"
      opacity={0.4}
    />
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

      {/* Topology Canvas */}
      <div className="relative min-h-[500px] overflow-hidden rounded-lg border border-border/20 bg-[#0b0b0b] p-6">
        {/* Background grid */}
        <div className="pointer-events-none absolute inset-0 opacity-[0.03]" style={{
          backgroundImage: 'radial-gradient(circle, var(--foreground) 1px, transparent 1px)',
          backgroundSize: '24px 24px',
        }} />

        {/* SVG connectors layer */}
        <svg className="pointer-events-none absolute inset-0 h-full w-full" preserveAspectRatio="none">
          {/* Eve → MCP servers (left side) */}
          {mcpServers.map((_, i) => (
            <Connector
              key={`mcp-${i}`}
              x1={380}
              y1={200}
              x2={120}
              y2={80 + i * 90}
            />
          ))}
          {/* Eve → Platforms (right side) */}
          {platforms.map((_, i) => (
            <Connector
              key={`plat-${i}`}
              x1={580}
              y1={200}
              x2={780}
              y2={80 + i * 80}
            />
          ))}
          {/* Eve → Schedules (bottom) */}
          {activeSchedules.map((_, i) => (
            <Connector
              key={`sched-${i}`}
              x1={480}
              y1={280}
              x2={300 + i * 200}
              y2={420}
            />
          ))}
        </svg>

        {/* Positioned nodes */}
        <div className="relative z-10">
          {/* Central Eve node */}
          <div className="absolute left-1/2 top-[140px] -translate-x-1/2">
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
                <Stat icon={Cpu} value={String(health?.total_tools ?? 0)} label="tools" />
                <Stat icon={Activity} value={String(health?.total_sessions ?? 0)} label="sessions" />
                <Stat icon={Clock} value={String(health?.active_schedules ?? 0)} label="schedules" />
              </div>
            </div>
          </div>

          {/* MCP Servers — left column */}
          {mcpServers.length > 0 && (
            <div className="absolute left-8 top-[40px] flex flex-col gap-2">
              <span className="mb-1 font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                MCP Services
              </span>
              {mcpServers.map((server) => (
                <TopoNode
                  key={server.name}
                  label={server.name}
                  subtitle={`${server.connected ? 'connected' : 'disconnected'}`}
                  status={server.connected ? 'online' : 'offline'}
                  icon={Server}
                  size="sm"
                />
              ))}
            </div>
          )}

          {/* Platform connections — right column */}
          {platforms.length > 0 && (
            <div className="absolute right-8 top-[40px] flex flex-col gap-2">
              <span className="mb-1 font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                Platforms
              </span>
              {platforms.map(([platform, count]) => (
                <TopoNode
                  key={platform}
                  label={platform}
                  subtitle={`${count} sessions`}
                  status="online"
                  icon={platform === 'api' ? Globe : platform.includes('webhook') ? Webhook : MessageSquare}
                  accent={getPlatformColor(platform)}
                  size="sm"
                />
              ))}
            </div>
          )}

          {/* Active schedules — bottom row */}
          {activeSchedules.length > 0 && (
            <div className="absolute bottom-4 left-1/2 -translate-x-1/2">
              <div className="flex flex-col items-center gap-2">
                <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
                  Active Schedules
                </span>
                <div className="flex gap-2">
                  {activeSchedules.slice(0, 4).map((schedule) => (
                    <TopoNode
                      key={schedule.id}
                      label={schedule.id}
                      subtitle={schedule.cron_expression}
                      status="idle"
                      icon={Clock}
                      size="sm"
                    />
                  ))}
                  {activeSchedules.length > 4 && (
                    <div className="flex items-center px-2 font-mono text-[9px] text-muted-foreground/30">
                      +{activeSchedules.length - 4} more
                    </div>
                  )}
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  )
}

function Stat({ icon: Icon, value, label }: { icon: React.ComponentType<{ className?: string }>; value: string; label: string }) {
  return (
    <div className="flex items-center gap-1.5">
      <Icon className="h-3 w-3 text-muted-foreground/30" />
      <span className="font-mono text-[10px] tabular-nums text-foreground/60">{value}</span>
      <span className="font-mono text-[8px] text-muted-foreground/30">{label}</span>
    </div>
  )
}

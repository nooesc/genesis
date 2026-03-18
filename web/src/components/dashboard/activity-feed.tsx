import { useAuditLog } from '@/lib/api/queries/audit'
import { formatRelativeTime } from '@/lib/utils'
import {
  MessageSquare, Wrench, Brain, Database,
  Clock, AlertTriangle, Activity, Zap,
} from 'lucide-react'
import type { AuditEntry } from '@/lib/api/types'
import { Skeleton } from '@/components/ui/skeleton'

function getActionIcon(action: string) {
  if (action.includes('session')) return MessageSquare
  if (action.includes('tool')) return Wrench
  if (action.includes('skill')) return Brain
  if (action.includes('memory')) return Database
  if (action.includes('schedule')) return Clock
  if (action.includes('error')) return AlertTriangle
  if (action.includes('llm') || action.includes('token')) return Zap
  return Activity
}

function getActionColor(action: string): string {
  if (action.includes('error') || action.includes('fail')) return 'text-red-400'
  if (action.includes('create') || action.includes('new')) return 'text-emerald-400'
  if (action.includes('delete') || action.includes('remove')) return 'text-amber-400'
  return 'text-muted-foreground'
}

function ActivityItem({ entry }: { entry: AuditEntry }) {
  const Icon = getActionIcon(entry.action)
  const color = getActionColor(entry.action)

  return (
    <div className="group flex items-start gap-2.5 py-1.5">
      <div className={`mt-0.5 ${color}`}>
        <Icon className="h-3 w-3" strokeWidth={1.5} />
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-mono text-[11px] text-foreground/80">
          {entry.action}
        </span>
        {entry.details && (
          <span className="truncate font-mono text-[10px] text-muted-foreground/50">
            {entry.details}
          </span>
        )}
      </div>
      <span className="shrink-0 font-mono text-[9px] tabular-nums text-muted-foreground/40">
        {formatRelativeTime(entry.created_at)}
      </span>
    </div>
  )
}

export function ActivityFeed() {
  const { data: entries, isLoading } = useAuditLog({ limit: 15, refetchInterval: 10_000 })

  if (isLoading) {
    return (
      <div className="flex flex-col gap-2">
        {Array.from({ length: 6 }).map((_, i) => (
          <Skeleton key={i} className="h-6 w-full rounded" />
        ))}
      </div>
    )
  }

  if (!entries || entries.length === 0) {
    return (
      <div className="flex h-full items-center justify-center font-mono text-xs text-muted-foreground/50">
        No activity yet
      </div>
    )
  }

  return (
    <div className="flex flex-col divide-y divide-border/30">
      {entries.map((entry) => (
        <ActivityItem key={entry.id} entry={entry} />
      ))}
    </div>
  )
}

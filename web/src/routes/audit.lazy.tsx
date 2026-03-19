import * as React from 'react'
import { createLazyFileRoute, Link } from '@tanstack/react-router'
import { useAuditLog } from '@/lib/api/queries/audit'
import { EmptyState } from '@/components/shared/empty-state'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import type { AuditEntry } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'
import {
  MessageSquare, Wrench, Brain, Database,
  Clock, AlertTriangle, Activity, Zap, Shield, Settings,
} from 'lucide-react'

export const Route = createLazyFileRoute('/audit')({
  component: AuditPage,
})

function getActionIcon(action: string) {
  if (action.includes('session')) return MessageSquare
  if (action.includes('tool')) return Wrench
  if (action.includes('skill')) return Brain
  if (action.includes('memory')) return Database
  if (action.includes('schedule')) return Clock
  if (action.includes('error') || action.includes('fail')) return AlertTriangle
  if (action.includes('llm') || action.includes('token')) return Zap
  if (action.includes('auth') || action.includes('pair')) return Shield
  if (action.includes('config') || action.includes('setting')) return Settings
  return Activity
}

function getActionColor(action: string): string {
  if (action.includes('error') || action.includes('fail')) return 'text-red-400 bg-red-400/10'
  if (action.includes('create') || action.includes('new')) return 'text-emerald-400 bg-emerald-400/10'
  if (action.includes('delete') || action.includes('remove')) return 'text-amber-400 bg-amber-400/10'
  return 'text-muted-foreground bg-muted/30'
}

function AuditEntry({ entry }: { entry: AuditEntry }) {
  const Icon = getActionIcon(entry.action)
  const colorClass = getActionColor(entry.action)
  const [iconColor, bgColor] = colorClass.split(' ')

  return (
    <div className="group flex items-start gap-3 py-2">
      {/* Icon */}
      <div className={`mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded ${bgColor}`}>
        <Icon className={`h-3 w-3 ${iconColor}`} strokeWidth={1.5} />
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="font-mono text-[11px] font-medium text-foreground/80">
            {entry.action}
          </span>
          {entry.session_id && (
            <Link
              to="/sessions/$id"
              params={{ id: entry.session_id }}
              className="font-mono text-[9px] text-primary/60 transition-colors hover:text-primary"
            >
              {entry.session_id.slice(0, 8)}
            </Link>
          )}
        </div>
        {entry.details && (
          <p className="mt-0.5 line-clamp-2 font-mono text-[10px] leading-relaxed text-muted-foreground/40">
            {entry.details}
          </p>
        )}
      </div>

      {/* Timestamp */}
      <span
        className="shrink-0 font-mono text-[9px] tabular-nums text-muted-foreground/30"
        title={new Date(entry.created_at).toLocaleString()}
      >
        {formatRelativeTime(entry.created_at)}
      </span>
    </div>
  )
}

function AuditPage() {
  const { data: entries, isLoading } = useAuditLog({ limit: 200 })
  const [search, setSearch] = React.useState('')
  const [actionFilter, setActionFilter] = React.useState('')

  const allActions = React.useMemo(() => {
    const actions = new Set<string>()
    for (const entry of entries ?? []) {
      actions.add(entry.action)
    }
    return Array.from(actions).sort()
  }, [entries])

  const filtered = React.useMemo(() => {
    return (entries ?? []).filter((entry) => {
      const matchesSearch =
        !search ||
        entry.action.toLowerCase().includes(search.toLowerCase()) ||
        (entry.session_id?.toLowerCase().includes(search.toLowerCase()) ?? false) ||
        (entry.details?.toLowerCase().includes(search.toLowerCase()) ?? false)
      const matchesAction = !actionFilter || entry.action === actionFilter
      return matchesSearch && matchesAction
    })
  }, [entries, search, actionFilter])

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Audit Log
          </h1>
          {!isLoading && (
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
              {filtered.length} entries
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 font-mono text-[11px]"
          />
          <select
            value={actionFilter}
            onChange={(e) => setActionFilter(e.target.value)}
            className="h-7 rounded-md border border-border/40 bg-card/30 px-2 font-mono text-[10px] text-foreground/60 focus:outline-none focus:ring-1 focus:ring-ring"
          >
            <option value="">All actions</option>
            {allActions.map((action) => (
              <option key={action} value={action}>
                {action}
              </option>
            ))}
          </select>
        </div>
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 10 }).map((_, i) => (
            <Skeleton key={i} className="h-12 w-full rounded" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          title="No audit entries"
          description={
            search || actionFilter
              ? 'Try adjusting your filters.'
              : 'No activity has been recorded yet.'
          }
        />
      ) : (
        <div className="divide-y divide-border/20">
          {filtered.map((entry) => (
            <AuditEntry key={entry.id} entry={entry} />
          ))}
        </div>
      )}
    </div>
  )
}

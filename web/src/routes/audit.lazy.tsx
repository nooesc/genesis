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

type ActionCategory = 'session' | 'tool' | 'skill' | 'memory' | 'schedule' | 'error' | 'llm' | 'auth' | 'config' | 'other'

function categorize(action: string): ActionCategory {
  if (action.includes('session')) return 'session'
  if (action.includes('tool')) return 'tool'
  if (action.includes('skill')) return 'skill'
  if (action.includes('memory')) return 'memory'
  if (action.includes('schedule')) return 'schedule'
  if (action.includes('error') || action.includes('fail')) return 'error'
  if (action.includes('llm') || action.includes('token')) return 'llm'
  if (action.includes('auth') || action.includes('pair')) return 'auth'
  if (action.includes('config') || action.includes('setting')) return 'config'
  return 'other'
}

const CATEGORY_META: Record<ActionCategory, { icon: typeof Activity; color: string }> = {
  session: { icon: MessageSquare, color: 'text-blue-400 bg-blue-400/10' },
  tool: { icon: Wrench, color: 'text-cyan-400 bg-cyan-400/10' },
  skill: { icon: Brain, color: 'text-purple-400 bg-purple-400/10' },
  memory: { icon: Database, color: 'text-emerald-400 bg-emerald-400/10' },
  schedule: { icon: Clock, color: 'text-amber-400 bg-amber-400/10' },
  error: { icon: AlertTriangle, color: 'text-red-400 bg-red-400/10' },
  llm: { icon: Zap, color: 'text-yellow-400 bg-yellow-400/10' },
  auth: { icon: Shield, color: 'text-indigo-400 bg-indigo-400/10' },
  config: { icon: Settings, color: 'text-gray-400 bg-gray-400/10' },
  other: { icon: Activity, color: 'text-muted-foreground bg-muted/30' },
}

function getActionColor(action: string): string {
  if (action.includes('error') || action.includes('fail')) return 'text-red-400'
  if (action.includes('create') || action.includes('new')) return 'text-emerald-400'
  if (action.includes('delete') || action.includes('remove')) return 'text-amber-400'
  return 'text-foreground/70'
}

function AuditRow({ entry, isLast }: { entry: AuditEntry; isLast: boolean }) {
  const category = categorize(entry.action)
  const meta = CATEGORY_META[category]
  const Icon = meta.icon
  const [iconColor, bgColor] = meta.color.split(' ')
  const actionColor = getActionColor(entry.action)

  return (
    <div className="group flex items-stretch gap-3">
      {/* Timeline rail */}
      <div className="flex w-6 flex-col items-center">
        <div className={`flex h-5 w-5 shrink-0 items-center justify-center rounded ${bgColor}`}>
          <Icon className={`h-2.5 w-2.5 ${iconColor}`} strokeWidth={1.5} />
        </div>
        {!isLast && <div className="w-px flex-1 bg-border/15" />}
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1 pb-3">
        <div className="flex items-center gap-2">
          <span className={`font-mono text-[11px] font-medium ${actionColor}`}>
            {entry.action}
          </span>
          {entry.session_id && (
            <Link
              to="/sessions/$id"
              params={{ id: entry.session_id }}
              className="font-mono text-[9px] text-primary/50 transition-colors hover:text-primary"
            >
              {entry.session_id.slice(0, 8)}
            </Link>
          )}
          <span
            className="ml-auto shrink-0 font-mono text-[8px] tabular-nums text-muted-foreground/25"
            title={new Date(entry.created_at).toLocaleString()}
          >
            {formatRelativeTime(entry.created_at)}
          </span>
        </div>
        {entry.details && (
          <p className="mt-0.5 line-clamp-2 font-mono text-[9px] leading-relaxed text-muted-foreground/35">
            {entry.details}
          </p>
        )}
      </div>
    </div>
  )
}

function AuditPage() {
  const { data: entries, isLoading } = useAuditLog({ limit: 200 }, { refetchInterval: 15_000 })
  const [search, setSearch] = React.useState('')
  const [categoryFilter, setCategoryFilter] = React.useState<ActionCategory | null>(null)

  const categoryCounts = React.useMemo(() => {
    const counts = new Map<ActionCategory, number>()
    for (const entry of entries ?? []) {
      const cat = categorize(entry.action)
      counts.set(cat, (counts.get(cat) ?? 0) + 1)
    }
    return counts
  }, [entries])

  const filtered = React.useMemo(() => {
    return (entries ?? []).filter((entry) => {
      const matchesSearch =
        !search ||
        entry.action.toLowerCase().includes(search.toLowerCase()) ||
        (entry.session_id?.toLowerCase().includes(search.toLowerCase()) ?? false) ||
        (entry.details?.toLowerCase().includes(search.toLowerCase()) ?? false)
      const matchesCategory = !categoryFilter || categorize(entry.action) === categoryFilter
      return matchesSearch && matchesCategory
    })
  }, [entries, search, categoryFilter])

  const activeCategories = React.useMemo(
    () =>
      (Object.keys(CATEGORY_META) as ActionCategory[]).filter(
        (cat) => (categoryCounts.get(cat) ?? 0) > 0,
      ),
    [categoryCounts],
  )

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Event Stream
          </h1>
          {!isLoading && (
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
              {filtered.length}
            </span>
          )}
        </div>
        <Input
          placeholder="Search..."
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          className="w-48 font-mono text-[11px]"
        />
      </div>

      {/* Category filter chips */}
      {!isLoading && activeCategories.length > 1 && (
        <div className="flex flex-wrap items-center gap-1.5">
          <button
            onClick={() => setCategoryFilter(null)}
            className={`rounded px-2 py-0.5 font-mono text-[9px] uppercase tracking-wider transition-colors ${
              !categoryFilter
                ? 'bg-primary/10 text-primary'
                : 'text-muted-foreground/40 hover:text-muted-foreground'
            }`}
          >
            All ({entries?.length ?? 0})
          </button>
          {activeCategories.map((cat) => {
            const meta = CATEGORY_META[cat]
            const Icon = meta.icon
            const count = categoryCounts.get(cat) ?? 0
            return (
              <button
                key={cat}
                onClick={() => setCategoryFilter(categoryFilter === cat ? null : cat)}
                className={`flex items-center gap-1 rounded px-2 py-0.5 font-mono text-[9px] uppercase tracking-wider transition-colors ${
                  categoryFilter === cat
                    ? 'bg-primary/10 text-primary'
                    : 'text-muted-foreground/40 hover:text-muted-foreground'
                }`}
              >
                <Icon className="h-2.5 w-2.5" strokeWidth={1.5} />
                {cat} ({count})
              </button>
            )
          })}
        </div>
      )}

      {/* Event timeline */}
      {isLoading ? (
        <div className="card-stagger flex flex-col gap-2">
          {Array.from({ length: 10 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded-md" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          title="No events"
          description={
            search || categoryFilter
              ? 'Try adjusting your filters.'
              : 'No activity has been recorded yet.'
          }
        />
      ) : (
        <div className="card-stagger">
          {filtered.map((entry, i) => (
            <AuditRow key={entry.id} entry={entry} isLast={i === filtered.length - 1} />
          ))}
        </div>
      )}
    </div>
  )
}

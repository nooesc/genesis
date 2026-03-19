import * as React from 'react'
import { createLazyFileRoute, Link } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { useAuditLog } from '@/lib/api/queries/audit'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import type { AuditEntry } from '@/lib/api/types'
import { formatRelativeTime, truncate } from '@/lib/utils'

export const Route = createLazyFileRoute('/audit')({
  component: AuditPage,
})

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
      const matchesAction =
        !actionFilter || entry.action === actionFilter
      return matchesSearch && matchesAction
    })
  }, [entries, search, actionFilter])

  const columns: ColumnDef<AuditEntry, unknown>[] = React.useMemo(
    () => [
      {
        accessorKey: 'created_at',
        header: 'Timestamp',
        cell: ({ row }) => (
          <span
            className="font-mono text-xs text-muted-foreground"
            title={new Date(row.original.created_at).toLocaleString()}
          >
            {formatRelativeTime(row.original.created_at)}
          </span>
        ),
      },
      {
        accessorKey: 'action',
        header: 'Action',
        cell: ({ row }) => (
          <span className="font-mono text-xs font-medium">{row.original.action}</span>
        ),
      },
      {
        accessorKey: 'session_id',
        header: 'Session',
        cell: ({ row }) => {
          const sid = row.original.session_id
          if (!sid) return <span className="font-mono text-xs text-muted-foreground">—</span>
          const short = sid.slice(0, 8)
          return (
            <Link
              to="/sessions/$id"
              params={{ id: sid }}
              className="font-mono text-xs text-primary underline-offset-4 hover:underline"
            >
              {short}…
            </Link>
          )
        },
      },
      {
        accessorKey: 'details',
        header: 'Details',
        cell: ({ row }) => {
          const details = row.original.details
          if (!details) return <span className="font-mono text-xs text-muted-foreground">—</span>
          return (
            <span className="font-mono text-xs text-muted-foreground" title={details}>
              {truncate(details, 120)}
            </span>
          )
        },
      },
    ],
    [],
  )

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Audit Log
        </h1>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search entries..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 font-mono text-xs"
          />
          <select
            value={actionFilter}
            onChange={(e) => setActionFilter(e.target.value)}
            className="h-8 rounded-md border border-input bg-background px-2 font-mono text-xs text-foreground focus:outline-none focus:ring-1 focus:ring-ring"
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
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded" />
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
        <DataTable columns={columns} data={filtered} />
      )}
    </div>
  )
}

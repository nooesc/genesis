import * as React from 'react'
import { createLazyFileRoute, useNavigate } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { useSessionsWithCount } from '@/lib/api/queries/sessions'
import { DataTable } from '@/components/shared/data-table'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { PageHeader } from '@/components/shared/page-header'
import type { SessionSummary } from '@/lib/api/types'
import { formatRelativeTime, formatTokens } from '@/lib/utils'
import { getPlatformColor } from '@/lib/platforms'
import { MessagesSquareIcon } from 'lucide-react'

export const Route = createLazyFileRoute('/sessions/')({
  component: SessionsPage,
})

const columns: ColumnDef<SessionSummary, unknown>[] = [
  {
    id: 'status',
    header: '',
    size: 28,
    cell: ({ row }) => {
      const color = getPlatformColor(row.original.platform)
      const ageMs = Date.now() - new Date(row.original.updated_at).getTime()
      const isRecent = ageMs < 3600_000 // last hour
      return (
        <div className="flex items-center justify-center">
          <div
            className="h-2 w-2 rounded-full"
            style={{
              backgroundColor: color,
              boxShadow: isRecent ? `0 0 6px ${color}60` : undefined,
            }}
          />
        </div>
      )
    },
  },
  {
    accessorKey: 'id',
    header: 'ID',
    size: 80,
    cell: ({ row }) => (
      <span className="font-mono text-[10px] tabular-nums text-muted-foreground/60">
        {row.original.id.slice(0, 8)}
      </span>
    ),
  },
  {
    accessorKey: 'title',
    header: 'Title',
    cell: ({ row }) => (
      <span className="max-w-[300px] truncate font-mono text-[11px]">
        {row.original.title ?? (
          <span className="italic text-muted-foreground/40">Untitled</span>
        )}
      </span>
    ),
  },
  {
    accessorKey: 'platform',
    header: 'Platform',
    size: 100,
    cell: ({ row }) => (
      <Badge variant="outline" className="font-mono text-[9px] uppercase">
        {row.original.platform}
      </Badge>
    ),
  },
  {
    id: 'tokens',
    header: 'Tokens',
    size: 120,
    accessorFn: (row) => row.total_input_tokens + row.total_output_tokens,
    cell: ({ row, table }) => {
      const total = row.original.total_input_tokens + row.original.total_output_tokens
      const maxTokens = (table.options.meta as { maxTokens?: number })?.maxTokens ?? 1
      const barWidth = maxTokens > 0 ? Math.min((total / maxTokens) * 100, 100) : 0
      return (
        <div className="flex items-center gap-2">
          <span className="w-12 text-right font-mono text-[10px] tabular-nums text-muted-foreground/60">
            {formatTokens(total)}
          </span>
          <div className="h-1 w-16 overflow-hidden rounded-full bg-border/30">
            <div
              className="h-full rounded-full bg-primary/40"
              style={{ width: `${barWidth}%` }}
            />
          </div>
        </div>
      )
    },
  },
  {
    accessorKey: 'updated_at',
    header: 'Updated',
    size: 90,
    cell: ({ row }) => (
      <span className="font-mono text-[10px] tabular-nums text-muted-foreground/40">
        {formatRelativeTime(row.original.updated_at)}
      </span>
    ),
  },
]

function SessionsPage() {
  const navigate = useNavigate({ from: '/sessions/' })
  const { search } = Route.useSearch()
  const [inputValue, setInputValue] = React.useState(search ?? '')
  const debounceRef = React.useRef<ReturnType<typeof setTimeout> | null>(null)

  const { data: response, isLoading } = useSessionsWithCount({ search: search || undefined })
  const sessions = response?.sessions
  const totalCount = response?.count ?? 0

  function handleSearchChange(e: React.ChangeEvent<HTMLInputElement>) {
    const value = e.target.value
    setInputValue(value)

    if (debounceRef.current) clearTimeout(debounceRef.current)
    debounceRef.current = setTimeout(() => {
      void navigate({
        search: (prev) => ({ ...prev, search: value || undefined }),
        replace: true,
      })
    }, 300)
  }

  React.useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current)
    }
  }, [])

  const maxTokens = React.useMemo(
    () => Math.max(1, ...(sessions ?? []).map(s => s.total_input_tokens + s.total_output_tokens)),
    [sessions],
  )

  return (
    <div className="flex flex-col gap-4">
      <PageHeader title="Sessions" icon={MessagesSquareIcon} count={isLoading ? undefined : totalCount}>
        <Input
          placeholder="Search..."
          value={inputValue}
          onChange={handleSearchChange}
          className="w-48 font-mono text-[11px]"
        />
      </PageHeader>

      {isLoading ? (
        <div className="flex flex-col gap-1.5">
          {Array.from({ length: 10 }).map((_, i) => (
            <Skeleton key={i} className="h-9 w-full rounded" />
          ))}
        </div>
      ) : (
        <DataTable
          columns={columns}
          data={sessions ?? []}
          meta={{ maxTokens }}
          onRowClick={(session) => {
            void navigate({ to: '/sessions/$id', params: { id: session.id } })
          }}
        />
      )}
    </div>
  )
}

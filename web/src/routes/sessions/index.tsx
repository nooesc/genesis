import * as React from 'react'
import { createFileRoute, useNavigate } from '@tanstack/react-router'
import { z } from 'zod'
import { type ColumnDef } from '@tanstack/react-table'
import { useSessions } from '@/lib/api/queries/sessions'
import { DataTable } from '@/components/shared/data-table'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import type { SessionSummary } from '@/lib/api/types'
import { formatRelativeTime, formatTokens } from '@/lib/utils'

const searchSchema = z.object({
  search: z.string().optional(),
})

export const Route = createFileRoute('/sessions/')({
  validateSearch: searchSchema,
  component: SessionsPage,
})

const columns: ColumnDef<SessionSummary, unknown>[] = [
  {
    accessorKey: 'id',
    header: 'ID',
    cell: ({ row }) => (
      <span className="font-mono text-xs text-muted-foreground">
        {row.original.id.slice(0, 8)}
      </span>
    ),
  },
  {
    accessorKey: 'title',
    header: 'Title',
    cell: ({ row }) => (
      <span className="max-w-[300px] truncate font-mono text-xs">
        {row.original.title ?? (
          <span className="italic text-muted-foreground">Untitled</span>
        )}
      </span>
    ),
  },
  {
    accessorKey: 'platform',
    header: 'Platform',
    cell: ({ row }) => (
      <Badge variant="outline" className="font-mono text-[10px]">
        {row.original.platform}
      </Badge>
    ),
  },
  {
    id: 'tokens',
    header: 'Tokens',
    accessorFn: (row) => row.total_input_tokens + row.total_output_tokens,
    cell: ({ row }) => (
      <span className="font-mono text-xs text-muted-foreground">
        {formatTokens(row.original.total_input_tokens + row.original.total_output_tokens)}
      </span>
    ),
  },
  {
    accessorKey: 'updated_at',
    header: 'Updated',
    cell: ({ row }) => (
      <span className="font-mono text-xs text-muted-foreground">
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

  const { data: sessions, isLoading } = useSessions({ search: search || undefined })

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

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Sessions
        </h1>
        <Input
          placeholder="Search sessions..."
          value={inputValue}
          onChange={handleSearchChange}
          className="w-64 font-mono text-xs"
        />
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded" />
          ))}
        </div>
      ) : (
        <DataTable
          columns={columns}
          data={sessions ?? []}
          onRowClick={(session) => {
            void navigate({ to: '/sessions/$id', params: { id: session.id } })
          }}
        />
      )}
    </div>
  )
}

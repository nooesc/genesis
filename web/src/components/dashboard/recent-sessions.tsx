import { useNavigate } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { DataTable } from '@/components/shared/data-table'
import { Badge } from '@/components/ui/badge'
import type { SessionSummary } from '@/lib/api/types'
import { formatRelativeTime, formatTokens } from '@/lib/utils'

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
      <span className="max-w-[200px] truncate font-mono text-xs">
        {row.original.title ?? <span className="italic text-muted-foreground">Untitled</span>}
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
    header: 'Time',
    cell: ({ row }) => (
      <span className="font-mono text-xs text-muted-foreground">
        {formatRelativeTime(row.original.updated_at)}
      </span>
    ),
  },
]

interface RecentSessionsProps {
  sessions: SessionSummary[]
}

export function RecentSessions({ sessions }: RecentSessionsProps) {
  const navigate = useNavigate()

  return (
    <DataTable
      columns={columns}
      data={sessions}
      onRowClick={(session) => {
        void navigate({ to: '/sessions/$id', params: { id: session.id } })
      }}
    />
  )
}

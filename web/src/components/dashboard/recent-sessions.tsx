import { useNavigate } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { DataTable } from '@/components/shared/data-table'
import { Badge } from '@/components/ui/badge'
import type { SessionSummary } from '@/lib/api/types'

function formatRelativeTime(isoString: string): string {
  const now = Date.now()
  const then = new Date(isoString).getTime()
  const diffMs = now - then
  const diffSec = Math.floor(diffMs / 1000)
  if (diffSec < 60) return `${diffSec}s ago`
  const diffMin = Math.floor(diffSec / 60)
  if (diffMin < 60) return `${diffMin}m ago`
  const diffHr = Math.floor(diffMin / 60)
  if (diffHr < 24) return `${diffHr}h ago`
  const diffDay = Math.floor(diffHr / 24)
  return `${diffDay}d ago`
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`
  return String(n)
}

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

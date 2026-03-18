import * as React from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { TrashIcon, DatabaseIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useMemories, useSearchMemories } from '@/lib/api/queries/memories'
import { useDeleteMemory, useEmbedAll } from '@/lib/api/mutations/memories'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
import { ConfirmDeleteDialog } from '@/components/shared/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import type { Memory } from '@/lib/api/types'
import { formatRelativeTime, truncate } from '@/lib/utils'

export const Route = createFileRoute('/memories')({
  component: MemoriesPage,
})


function MemoriesPage() {
  const [search, setSearch] = React.useState('')
  const [debouncedSearch, setDebouncedSearch] = React.useState('')
  const debounceRef = React.useRef<ReturnType<typeof setTimeout> | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Memory | null>(null)
  const embedAll = useEmbedAll()
  const deleteMemory = useDeleteMemory()

  React.useEffect(() => {
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current)
    }
  }, [])

  function handleSearchChange(e: React.ChangeEvent<HTMLInputElement>) {
    const value = e.target.value
    setSearch(value)
    if (debounceRef.current) clearTimeout(debounceRef.current)
    debounceRef.current = setTimeout(() => {
      setDebouncedSearch(value)
    }, 300)
  }

  const memoriesQuery = useMemories()
  const searchQuery = useSearchMemories(debouncedSearch)

  const isSearching = Boolean(debouncedSearch)
  const { data: memories, isLoading } = isSearching ? searchQuery : memoriesQuery

  function handleEmbedAll() {
    const contents = (memories ?? []).map((m) => m.content)
    if (contents.length === 0) {
      toast.info('No memories to embed')
      return
    }
    embedAll.mutate(contents, {
      onSuccess: (result) => {
        toast.success(`Embedded ${result.embedded} memories`)
      },
      onError: (err) => {
        toast.error(`Failed to embed: ${err.message}`)
      },
    })
  }

  const columns: ColumnDef<Memory, unknown>[] = React.useMemo(
    () => [
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
        accessorKey: 'content',
        header: 'Content',
        cell: ({ row }) => (
          <span className="font-mono text-xs">{truncate(row.original.content)}</span>
        ),
      },
      {
        accessorKey: 'source',
        header: 'Source',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.source || '—'}
          </span>
        ),
      },
      {
        accessorKey: 'created_at',
        header: 'Created',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {formatRelativeTime(row.original.created_at)}
          </span>
        ),
      },
      {
        id: 'actions',
        header: '',
        cell: ({ row }) => (
          <div className="flex justify-end">
            <Button
              variant="ghost"
              size="icon-sm"
              onClick={() => setDeleteTarget(row.original)}
            >
              <TrashIcon className="size-3.5 text-muted-foreground hover:text-destructive" />
              <span className="sr-only">Delete</span>
            </Button>
          </div>
        ),
      },
    ],
    [],
  )

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Memories
        </h1>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search memories..."
            value={search}
            onChange={handleSearchChange}
            className="w-56 font-mono text-xs"
          />
          <Button
            variant="outline"
            size="sm"
            className="font-mono text-xs gap-1.5"
            onClick={handleEmbedAll}
            disabled={embedAll.isPending}
          >
            <DatabaseIcon className="size-3" />
            {embedAll.isPending ? 'Embedding...' : 'Embed All'}
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 8 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded" />
          ))}
        </div>
      ) : (memories ?? []).length === 0 ? (
        <EmptyState
          title="No memories found"
          description={isSearching ? 'No results for your search.' : 'No memories stored yet.'}
        />
      ) : (
        <DataTable columns={columns} data={memories ?? []} />
      )}

      <ConfirmDeleteDialog
        open={Boolean(deleteTarget)}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget) return
          deleteMemory.mutate(deleteTarget.id, {
            onSuccess: () => {
              toast.success('Memory deleted')
              setDeleteTarget(null)
            },
            onError: (err) => {
              toast.error(`Failed to delete: ${err.message}`)
            },
          })
        }}
        isPending={deleteMemory.isPending}
        title="Delete Memory"
        description={`Delete memory ${deleteTarget?.id.slice(0, 8) ?? ''}? This cannot be undone.`}
      />
    </div>
  )
}

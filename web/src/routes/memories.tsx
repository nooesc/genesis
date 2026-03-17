import * as React from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { TrashIcon, DatabaseIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useMemories, useSearchMemories } from '@/lib/api/queries/memories'
import { useDeleteMemory, useEmbedAll } from '@/lib/api/mutations/memories'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import type { Memory } from '@/lib/api/types'

export const Route = createFileRoute('/memories')({
  component: MemoriesPage,
})

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

function truncate(text: string, max = 100): string {
  if (text.length <= max) return text
  return text.slice(0, max) + '…'
}

interface DeleteDialogProps {
  memory: Memory | null
  onClose: () => void
}

function DeleteDialog({ memory, onClose }: DeleteDialogProps) {
  const deleteMemory = useDeleteMemory()

  function handleDelete() {
    if (!memory) return
    deleteMemory.mutate(memory.id, {
      onSuccess: () => {
        toast.success('Memory deleted')
        onClose()
      },
      onError: (err) => {
        toast.error(`Failed to delete: ${err.message}`)
      },
    })
  }

  return (
    <Dialog open={Boolean(memory)} onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Delete Memory</DialogTitle>
        </DialogHeader>
        <p className="font-mono text-xs text-muted-foreground">
          Delete memory <span className="text-foreground">{memory?.id.slice(0, 8)}</span>? This cannot be undone.
        </p>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={handleDelete}
            disabled={deleteMemory.isPending}
          >
            {deleteMemory.isPending ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function MemoriesPage() {
  const [search, setSearch] = React.useState('')
  const [debouncedSearch, setDebouncedSearch] = React.useState('')
  const debounceRef = React.useRef<ReturnType<typeof setTimeout> | null>(null)
  const [deleteTarget, setDeleteTarget] = React.useState<Memory | null>(null)
  const embedAll = useEmbedAll()

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

      <DeleteDialog memory={deleteTarget} onClose={() => setDeleteTarget(null)} />
    </div>
  )
}

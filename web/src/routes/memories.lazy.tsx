import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import { TrashIcon, DatabaseIcon, ChevronDown, ChevronRight } from 'lucide-react'
import { toast } from 'sonner'
import { useMemories, useSearchMemories } from '@/lib/api/queries/memories'
import { useDeleteMemory, useEmbedAll } from '@/lib/api/mutations/memories'
import { EmptyState } from '@/components/shared/empty-state'
import { ConfirmDeleteDialog } from '@/components/shared/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { Badge } from '@/components/ui/badge'
import type { Memory } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'

export const Route = createLazyFileRoute('/memories')({
  component: MemoriesPage,
})

function MemoryCard({ memory, onDelete }: { memory: Memory; onDelete: () => void }) {
  const [expanded, setExpanded] = React.useState(false)
  const isLong = memory.content.length > 200

  return (
    <div className="group rounded-md border border-border/30 bg-card/30 p-3 transition-colors hover:border-border/50">
      {/* Header: source + time + actions */}
      <div className="mb-2 flex items-center gap-2">
        {memory.source && (
          <Badge variant="outline" className="font-mono text-[9px] uppercase">
            {memory.source}
          </Badge>
        )}
        <span className="font-mono text-[9px] tabular-nums text-muted-foreground/40">
          {memory.id.slice(0, 8)}
        </span>
        <span className="ml-auto font-mono text-[9px] tabular-nums text-muted-foreground/30">
          {formatRelativeTime(memory.created_at)}
        </span>
        <Button
          variant="ghost"
          size="icon-sm"
          className="h-5 w-5 opacity-0 transition-opacity group-hover:opacity-100"
          onClick={onDelete}
        >
          <TrashIcon className="size-3 text-muted-foreground hover:text-destructive" />
          <span className="sr-only">Delete</span>
        </Button>
      </div>

      {/* Content */}
      <p className={`whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-foreground/70 ${!expanded && isLong ? 'line-clamp-3' : ''}`}>
        {memory.content}
      </p>

      {/* Expand toggle */}
      {isLong && (
        <button
          onClick={() => setExpanded(v => !v)}
          className="mt-1.5 flex items-center gap-1 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          {expanded ? 'collapse' : 'expand'}
        </button>
      )}
    </div>
  )
}

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

  const sources = React.useMemo(() => {
    const counts = new Map<string, number>()
    for (const m of memories ?? []) {
      const src = m.source || 'unknown'
      counts.set(src, (counts.get(src) ?? 0) + 1)
    }
    return Array.from(counts.entries()).sort(([, a], [, b]) => b - a)
  }, [memories])

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

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Memories
          </h1>
          {!isLoading && (
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
              {memories?.length ?? 0}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search..."
            value={search}
            onChange={handleSearchChange}
            className="w-48 font-mono text-[11px]"
          />
          <Button
            variant="outline"
            size="sm"
            className="h-7 gap-1.5 px-2 font-mono text-[10px]"
            onClick={handleEmbedAll}
            disabled={embedAll.isPending}
          >
            <DatabaseIcon className="size-3" />
            {embedAll.isPending ? 'Embedding...' : 'Embed All'}
          </Button>
        </div>
      </div>

      {/* Source filter strip */}
      {!isLoading && sources.length > 1 && (
        <div className="flex items-center gap-1.5">
          <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/30">
            Sources
          </span>
          {sources.map(([source, count]) => (
            <span key={source} className="font-mono text-[9px] text-muted-foreground/40">
              {source} ({count})
            </span>
          ))}
        </div>
      )}

      {/* Memory cards */}
      {isLoading ? (
        <div className="grid grid-cols-1 gap-2 lg:grid-cols-2">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-24 w-full rounded-md" />
          ))}
        </div>
      ) : (memories ?? []).length === 0 ? (
        <EmptyState
          title="No memories found"
          description={isSearching ? 'No results for your search.' : 'No memories stored yet.'}
        />
      ) : (
        <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
          {(memories ?? []).map((memory) => (
            <MemoryCard
              key={memory.id}
              memory={memory}
              onDelete={() => setDeleteTarget(memory)}
            />
          ))}
        </div>
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

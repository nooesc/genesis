import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { useTools } from '@/lib/api/queries/tools'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { FilterIcon } from 'lucide-react'
import type { ToolInfo } from '@/lib/api/types'
import { truncate } from '@/lib/utils'

export const Route = createLazyFileRoute('/tools')({
  component: ToolsPage,
})

function ToolsPage() {
  const { data: tools, isLoading } = useTools()
  const [search, setSearch] = React.useState('')
  const [sourceFilter, setSourceFilter] = React.useState<string | null>(null)

  const allSources = React.useMemo(() => {
    const sources = new Set<string>()
    for (const tool of tools ?? []) {
      sources.add(tool.source)
    }
    return Array.from(sources).sort()
  }, [tools])

  const filtered = React.useMemo(() => {
    return (tools ?? []).filter((tool) => {
      const matchesSearch =
        !search ||
        tool.name.toLowerCase().includes(search.toLowerCase()) ||
        tool.description.toLowerCase().includes(search.toLowerCase())
      const matchesSource = !sourceFilter || tool.source === sourceFilter
      return matchesSearch && matchesSource
    })
  }, [tools, search, sourceFilter])

  const columns: ColumnDef<ToolInfo, unknown>[] = React.useMemo(
    () => [
      {
        accessorKey: 'name',
        header: 'Name',
        cell: ({ row }) => (
          <span className="font-mono text-xs font-medium">{row.original.name}</span>
        ),
      },
      {
        accessorKey: 'description',
        header: 'Description',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {truncate(row.original.description)}
          </span>
        ),
      },
      {
        accessorKey: 'source',
        header: 'Source',
        cell: ({ row }) => {
          const source = row.original.source
          const isBuiltin = source === 'builtin'
          return (
            <Badge variant={isBuiltin ? 'secondary' : 'outline'} className="font-mono text-[10px]">
              {source}
            </Badge>
          )
        },
      },
    ],
    [],
  )

  const groupedBySource = React.useMemo(() => {
    const groups: Record<string, ToolInfo[]> = {}
    for (const tool of filtered) {
      if (!groups[tool.source]) groups[tool.source] = []
      groups[tool.source].push(tool)
    }
    return groups
  }, [filtered])

  const sourceCount = allSources.length
  const showGrouped = !search && !sourceFilter && sourceCount > 1

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Tools Registry
        </h1>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search tools..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 font-mono text-xs"
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm" className="gap-1.5 font-mono text-xs">
                <FilterIcon className="size-3" />
                {sourceFilter ?? 'All sources'}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setSourceFilter(null)}>
                <span className="font-mono text-xs">All sources</span>
              </DropdownMenuItem>
              {allSources.map((src) => (
                <DropdownMenuItem key={src} onSelect={() => setSourceFilter(src)}>
                  <span className="font-mono text-xs">{src}</span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
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
          title="No tools found"
          description={
            search || sourceFilter
              ? 'Try adjusting your filters.'
              : 'No tools are registered.'
          }
        />
      ) : showGrouped ? (
        <div className="flex flex-col gap-6">
          {Object.entries(groupedBySource)
            .sort(([a], [b]) => {
              if (a === 'builtin') return -1
              if (b === 'builtin') return 1
              return a.localeCompare(b)
            })
            .map(([source, sourceTools]) => (
              <div key={source} className="flex flex-col gap-2">
                <div className="flex items-center gap-2">
                  <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
                    {source}
                  </span>
                  <span className="font-mono text-[10px] text-muted-foreground/50">
                    ({sourceTools.length})
                  </span>
                </div>
                <DataTable columns={columns} data={sourceTools} />
              </div>
            ))}
        </div>
      ) : (
        <DataTable columns={columns} data={filtered} />
      )}
    </div>
  )
}

import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import { useTools } from '@/lib/api/queries/tools'
import { EmptyState } from '@/components/shared/empty-state'
import { Badge } from '@/components/ui/badge'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import {
  WrenchIcon,
  ChevronDown,
  ChevronRight,
  ServerIcon,
  CpuIcon,
} from 'lucide-react'
import type { ToolInfo, ToolParameter } from '@/lib/api/types'

export const Route = createLazyFileRoute('/tools')({
  component: ToolsPage,
})

/** Renders a single parameter row with type info */
function ParamRow({
  name,
  param,
  required,
}: {
  name: string
  param: ToolParameter
  required: boolean
}) {
  return (
    <div className="flex items-start gap-2 py-0.5">
      <span className="font-mono text-[10px] font-medium text-foreground/70">
        {name}
        {required && <span className="text-destructive/60">*</span>}
      </span>
      <span className="font-mono text-[9px] text-muted-foreground/40">
        {param.type}
        {param.enum && ` [${param.enum.join(' | ')}]`}
        {param.items && param.type === 'array' && ` <${param.items.type}>`}
      </span>
      {param.description && (
        <span className="ml-auto max-w-[50%] text-right font-mono text-[9px] leading-tight text-muted-foreground/30">
          {param.description.length > 80
            ? param.description.slice(0, 80) + '…'
            : param.description}
        </span>
      )}
    </div>
  )
}

function ToolCard({ tool }: { tool: ToolInfo }) {
  const [expanded, setExpanded] = React.useState(false)

  const paramEntries = Object.entries(tool.parameters?.properties ?? {})
  const requiredSet = new Set(tool.parameters?.required ?? [])
  const paramCount = paramEntries.length
  const isBuiltin = tool.source === 'builtin'

  return (
    <div className="group rounded-md border border-border/30 bg-card/30 p-3 transition-colors hover:border-border/50">
      {/* Header: icon + name + source badge */}
      <div className="mb-1 flex items-start gap-2">
        <WrenchIcon className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground/30" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="font-mono text-[11px] font-medium text-foreground/80">
              {tool.name}
            </span>
            {paramCount > 0 && (
              <span className="font-mono text-[8px] tabular-nums text-muted-foreground/30">
                {paramCount} param{paramCount !== 1 ? 's' : ''}
              </span>
            )}
          </div>
          {tool.description && (
            <div className="mt-0.5 font-mono text-[10px] leading-relaxed text-muted-foreground/50">
              {tool.description.length > 120
                ? tool.description.slice(0, 120) + '…'
                : tool.description}
            </div>
          )}
        </div>
        <Badge
          variant={isBuiltin ? 'secondary' : 'outline'}
          className="shrink-0 font-mono text-[8px] uppercase"
        >
          {tool.source}
        </Badge>
      </div>

      {/* Expandable parameter schema */}
      {paramCount > 0 && (
        <button
          onClick={() => setExpanded((v) => !v)}
          className="mt-1 flex items-center gap-1 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground"
        >
          {expanded ? (
            <ChevronDown className="h-3 w-3" />
          ) : (
            <ChevronRight className="h-3 w-3" />
          )}
          {expanded ? 'hide parameters' : 'show parameters'}
        </button>
      )}
      {expanded && paramCount > 0 && (
        <div className="mt-1.5 rounded bg-muted/20 px-2 py-1.5">
          {paramEntries.map(([name, param]) => (
            <ParamRow
              key={name}
              name={name}
              param={param}
              required={requiredSet.has(name)}
            />
          ))}
        </div>
      )}
    </div>
  )
}

function SourceGroup({
  source,
  tools,
  icon,
}: {
  source: string
  tools: ToolInfo[]
  icon: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        {icon}
        <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
          {source}
        </span>
        <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
          {tools.length}
        </span>
        <div className="h-px flex-1 bg-border/20" />
      </div>
      <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
        {tools.map((tool) => (
          <ToolCard key={`${tool.source}:${tool.name}`} tool={tool} />
        ))}
      </div>
    </div>
  )
}

function ToolsPage() {
  const { data: tools, isLoading } = useTools()
  const [search, setSearch] = React.useState('')
  const [sourceFilter, setSourceFilter] = React.useState<string | null>(null)

  const sourceCounts = React.useMemo(() => {
    const counts = new Map<string, number>()
    for (const tool of tools ?? []) {
      counts.set(tool.source, (counts.get(tool.source) ?? 0) + 1)
    }
    return counts
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

  const groupedBySource = React.useMemo(() => {
    const groups: Record<string, ToolInfo[]> = {}
    for (const tool of filtered) {
      if (!groups[tool.source]) groups[tool.source] = []
      groups[tool.source].push(tool)
    }
    return Object.entries(groups).sort(([a], [b]) => {
      if (a === 'builtin') return -1
      if (b === 'builtin') return 1
      return a.localeCompare(b)
    })
  }, [filtered])

  const allSources = Array.from(sourceCounts.keys()).sort()

  return (
    <div className="flex flex-col gap-4">
      {/* Header */}
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Tools Registry
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

      {/* Source filter chips */}
      {!isLoading && allSources.length > 1 && (
        <div className="flex flex-wrap items-center gap-1.5">
          <button
            onClick={() => setSourceFilter(null)}
            className={`rounded px-2 py-0.5 font-mono text-[9px] uppercase tracking-wider transition-colors ${
              !sourceFilter
                ? 'bg-primary/10 text-primary'
                : 'text-muted-foreground/40 hover:text-muted-foreground'
            }`}
          >
            All ({tools?.length ?? 0})
          </button>
          {allSources.map((src) => (
            <button
              key={src}
              onClick={() =>
                setSourceFilter(sourceFilter === src ? null : src)
              }
              className={`rounded px-2 py-0.5 font-mono text-[9px] uppercase tracking-wider transition-colors ${
                sourceFilter === src
                  ? 'bg-primary/10 text-primary'
                  : 'text-muted-foreground/40 hover:text-muted-foreground'
              }`}
            >
              {src} ({sourceCounts.get(src) ?? 0})
            </button>
          ))}
        </div>
      )}

      {/* Tool groups */}
      {isLoading ? (
        <div className="flex flex-col gap-6">
          {Array.from({ length: 2 }).map((_, gi) => (
            <div key={gi} className="flex flex-col gap-2">
              <Skeleton className="h-4 w-32 rounded" />
              <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
                {Array.from({ length: 6 }).map((_, i) => (
                  <Skeleton key={i} className="h-20 w-full rounded-md" />
                ))}
              </div>
            </div>
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
      ) : (
        <div className="flex flex-col gap-6">
          {groupedBySource.map(([source, sourceTools]) => (
            <SourceGroup
              key={source}
              source={source}
              tools={sourceTools}
              icon={
                source === 'builtin' ? (
                  <CpuIcon className="h-3 w-3 text-muted-foreground/40" />
                ) : (
                  <ServerIcon className="h-3 w-3 text-muted-foreground/40" />
                )
              }
            />
          ))}
        </div>
      )}
    </div>
  )
}

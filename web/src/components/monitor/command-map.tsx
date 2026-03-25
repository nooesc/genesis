import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { cn } from '@/lib/utils'
import { useMemo } from 'react'
import type { CommandMapModel, CommandMapNode } from '@/lib/command-map/types'
import { CommandMapToolbar } from './command-map-toolbar'
import { CommandMapInspector } from './command-map-inspector'
import { useCommandMapState } from './use-command-map-state'

interface CommandMapProps {
  model: CommandMapModel
}

function nodeTone(node: CommandMapNode): string {
  if (node.kind === 'eve') return 'border-emerald-400/40 bg-emerald-400/10 text-emerald-100'
  if (node.kind === 'alert') return 'border-red-400/40 bg-red-400/10 text-red-100'
  if (node.kind === 'trigger') return 'border-amber-400/40 bg-amber-400/10 text-amber-100'
  return 'border-border/30 bg-card/60 text-foreground/90'
}

export function CommandMap({ model }: CommandMapProps) {
  const {
    selectedNode,
    isDecluttered,
    isFocused,
    visibleLayers,
    visibleNodes,
    selectNode,
    toggleLayer,
    toggleDeclutter,
    toggleFocus,
    resetView,
  } = useCommandMapState(model.nodes)

  const edges = model.edges.filter(edge => visibleNodes.some(node => node.id === edge.source) && visibleNodes.some(node => node.id === edge.target))
  const connectedNodeIds = useMemo(() => {
    if (!selectedNode || !isFocused) return new Set<string>()

    const connected = new Set<string>([selectedNode.id])
    for (const edge of edges) {
      if (edge.source === selectedNode.id) connected.add(edge.target)
      if (edge.target === selectedNode.id) connected.add(edge.source)
    }

    return connected
  }, [edges, isFocused, selectedNode])
  const displayedEdges = isFocused && selectedNode ? edges.filter(edge => edge.source === selectedNode.id || edge.target === selectedNode.id) : edges

  return (
    <div className="flex min-h-[calc(100vh-9rem)] flex-col gap-4">
      <CommandMapToolbar
        visibleLayers={visibleLayers}
        isDecluttered={isDecluttered}
        isFocused={isFocused}
        onToggleLayer={toggleLayer}
        onToggleDeclutter={toggleDeclutter}
        onToggleFocus={toggleFocus}
        onReset={resetView}
      />

      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <Card className="min-h-[32rem]">
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
              Command Map
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="grid gap-3 md:grid-cols-2 xl:grid-cols-3">
              {visibleNodes.map(node => (
                <Button
                  key={node.id}
                  type="button"
                  variant={node.id === selectedNode?.id ? 'default' : 'outline'}
                  onClick={() => selectNode(node.id)}
                  className={cn(
                    'h-auto min-h-24 flex-col items-start justify-start gap-1 rounded-xl border p-3 text-left font-mono',
                    nodeTone(node),
                    node.id === selectedNode?.id && 'ring-2 ring-primary/40',
                    isFocused && selectedNode && !connectedNodeIds.has(node.id) && 'opacity-45',
                  )}
                  aria-pressed={node.id === selectedNode?.id}
                  aria-label={node.label}
                >
                  <span className="text-[10px] uppercase tracking-[0.18em] text-muted-foreground/70">
                    {node.kind} · ring {node.ring}
                  </span>
                  <span className="text-base font-semibold">{node.label}</span>
                  {node.subtitle && <span className="text-xs text-muted-foreground/70">{node.subtitle}</span>}
                </Button>
              ))}
            </div>

            <div className="rounded-xl border border-border/20 bg-background/40 p-3 font-mono text-xs text-muted-foreground/60">
              <div className="mb-2 uppercase tracking-[0.2em] text-muted-foreground/50">Edges</div>
              {displayedEdges.length > 0 ? (
                <ul className="space-y-1">
                  {displayedEdges.map(edge => (
                    <li key={edge.id}>
                      {edge.source} → {edge.target}
                    </li>
                  ))}
                </ul>
              ) : (
                <p>No active edges in the current layer selection.</p>
              )}
            </div>
          </CardContent>
        </Card>

        <CommandMapInspector selectedNode={selectedNode} />
      </div>
    </div>
  )
}

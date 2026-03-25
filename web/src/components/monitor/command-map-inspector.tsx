import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import type { CommandMapNode } from '@/lib/command-map/types'

interface CommandMapInspectorProps {
  selectedNode: CommandMapNode | null
}

export function CommandMapInspector({ selectedNode }: CommandMapInspectorProps) {
  if (!selectedNode) {
    return (
      <Card className="h-full">
        <CardHeader className="pb-2">
          <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
            Inspector
          </CardTitle>
        </CardHeader>
        <CardContent className="font-mono text-sm text-muted-foreground/60">
          Select a node to inspect
        </CardContent>
      </Card>
    )
  }

  return (
    <Card className="h-full">
      <CardHeader className="pb-2">
        <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
          Inspector
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="space-y-1">
          <h2 className="font-mono text-base font-semibold text-foreground">{selectedNode.label}</h2>
          {selectedNode.subtitle && (
            <p className="font-mono text-xs text-muted-foreground/70">{selectedNode.subtitle}</p>
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Badge variant="secondary" className="font-mono text-[10px] uppercase tracking-[0.18em]">
            {selectedNode.kind}
          </Badge>
          <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
            ring {selectedNode.ring}
          </Badge>
        </div>
        <dl className="grid grid-cols-2 gap-2 font-mono text-xs">
          <div className="rounded-lg border border-border/20 bg-background/40 p-2">
            <dt className="text-muted-foreground/50">ID</dt>
            <dd className="truncate text-foreground/80">{selectedNode.id}</dd>
          </div>
          <div className="rounded-lg border border-border/20 bg-background/40 p-2">
            <dt className="text-muted-foreground/50">Status</dt>
            <dd className="text-foreground/80">{selectedNode.status ?? 'unknown'}</dd>
          </div>
        </dl>
      </CardContent>
    </Card>
  )
}

import type { Node, NodeProps, NodeTypes } from '@xyflow/react'
import { Handle, Position } from '@xyflow/react'
import { Pin, PinOff } from 'lucide-react'
import { memo } from 'react'
import { cn } from '@/lib/utils'
import type { CommandMapNode } from '@/lib/command-map/types'

export interface CommandMapCanvasNodeData extends Record<string, unknown> {
  node: CommandMapNode
  isDimmed: boolean
  isSelected: boolean
  onSelectNode: (nodeId: string) => void
  onTogglePinned: (nodeId: string) => void
}

export type CommandMapFlowNode = Node<CommandMapCanvasNodeData, 'commandMapNode'>

function commandMapNodeTone(node: CommandMapNode): string {
  if (node.kind === 'eve') {
    if (node.status === 'error') return 'border-red-400/40 bg-red-400/10 text-red-100'
    if (node.status === 'warning') return 'border-amber-400/40 bg-amber-400/10 text-amber-100'
    return 'border-emerald-400/40 bg-emerald-400/10 text-emerald-100'
  }
  if (node.kind === 'recipe') return 'border-sky-400/40 bg-sky-400/10 text-sky-100'
  if (node.kind === 'alert') return 'border-red-400/40 bg-red-400/10 text-red-100'
  if (node.kind === 'trigger') return 'border-amber-400/40 bg-amber-400/10 text-amber-100'
  return 'border-border/30 bg-card/60 text-foreground/90'
}

const CommandMapCanvasNode = memo(({ data }: NodeProps<CommandMapFlowNode>) => {
  const { node, isDimmed, isSelected, onSelectNode, onTogglePinned } = data
  const canPin = node.kind !== 'eve'

  return (
    <div className={cn('min-w-[220px] max-w-[260px] transition-opacity', isDimmed && 'opacity-45')}>
      <Handle
        type="target"
        position={Position.Top}
        isConnectable={false}
        className="!pointer-events-none !size-2 !border-0 !bg-primary/30 !opacity-0"
      />
      <div className="mb-1 flex items-center justify-between px-1 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-foreground/60">
        <span>
          {node.kind} · ring {node.ring}
        </span>
        {canPin ? (
          <button
            type="button"
            aria-label={node.pinned ? 'Unpin node' : 'Pin node'}
            onClick={(event) => {
              event.stopPropagation()
              onTogglePinned(node.id)
            }}
            className="rounded-md border border-border/30 bg-background/60 p-1 text-foreground/70 transition-colors hover:bg-background hover:text-foreground"
          >
            {node.pinned ? <PinOff className="size-3" /> : <Pin className="size-3" />}
          </button>
        ) : (
          <span className="px-1 text-muted-foreground/40">core</span>
        )}
      </div>

      <button
        type="button"
        onClick={() => onSelectNode(node.id)}
        aria-pressed={isSelected}
        className={cn(
          'w-full rounded-xl border p-3 text-left font-mono shadow-sm transition-all',
          commandMapNodeTone(node),
          isSelected && 'ring-2 ring-primary/40',
        )}
      >
        <div className="text-base font-semibold">{node.label}</div>
        {node.subtitle && <div className="mt-1 text-xs text-muted-foreground/70">{node.subtitle}</div>}
      </button>

      <Handle
        type="source"
        position={Position.Bottom}
        isConnectable={false}
        className="!pointer-events-none !size-2 !border-0 !bg-primary/30 !opacity-0"
      />
    </div>
  )
})

CommandMapCanvasNode.displayName = 'CommandMapCanvasNode'

export const commandMapNodeTypes: NodeTypes = {
  commandMapNode: CommandMapCanvasNode,
}

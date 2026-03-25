import { Background, BackgroundVariant, Controls, ReactFlow } from '@xyflow/react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { useMemo } from 'react'
import type { CommandMapModel } from '@/lib/command-map/types'
import { CommandMapToolbar } from './command-map-toolbar'
import { CommandMapInspector } from './command-map-inspector'
import { useCommandMapState } from './use-command-map-state'
import { commandMapNodeTypes } from './node-renderers'
import { useNodeLayout } from './use-node-layout'

interface CommandMapProps {
  model: CommandMapModel
  focusNodeId?: string | null
  focusMode?: 'focus' | 'select'
}

export function CommandMap({ model, focusNodeId = null, focusMode = 'select' }: CommandMapProps) {
  const {
    selectedNodeId,
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
  } = useCommandMapState(model.nodes, { focusNodeId, focusMode })

  const edges = useMemo(() => {
    const visibleNodeIds = new Set(visibleNodes.map(node => node.id))
    return model.edges.filter(edge => visibleNodeIds.has(edge.source) && visibleNodeIds.has(edge.target))
  }, [model.edges, visibleNodes])

  const {
    flowNodes,
    flowEdges,
    onNodesChange,
    onEdgesChange,
    onNodeDragStop,
    resetLayout,
    hasPinnedNodes,
  } = useNodeLayout({
    nodes: visibleNodes,
    allNodes: model.nodes,
    edges,
    selectedNodeId,
    isFocused,
    onSelectNode: selectNode,
  })

  function handleReset() {
    resetView()
    resetLayout()
  }

  return (
    <div className="flex min-h-[calc(100vh-9rem)] flex-col gap-4">
      <CommandMapToolbar
        visibleLayers={visibleLayers}
        isDecluttered={isDecluttered}
        isFocused={isFocused}
        canFocus={selectedNode !== null}
        onToggleLayer={toggleLayer}
        onToggleDeclutter={toggleDeclutter}
        onToggleFocus={toggleFocus}
        onReset={handleReset}
      />

      <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_20rem]">
        <Card className="min-h-[32rem]">
          <CardHeader className="pb-2">
            <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
              Command Map
            </CardTitle>
          </CardHeader>
          <CardContent className="relative h-[36rem] overflow-hidden p-0">
            <div className="pointer-events-none absolute left-4 top-4 z-10 rounded-lg border border-border/20 bg-background/70 px-3 py-2 font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground/70 backdrop-blur">
              {flowNodes.length} nodes · {flowEdges.length} edges
              {hasPinnedNodes && <span className="ml-2 text-foreground/80">Pinned layout active</span>}
            </div>

            <ReactFlow
              nodes={flowNodes}
              edges={flowEdges}
              nodeTypes={commandMapNodeTypes}
              onNodesChange={onNodesChange}
              onEdgesChange={onEdgesChange}
              onNodeDragStop={onNodeDragStop}
              fitView
              minZoom={0.35}
              maxZoom={1.8}
              nodesConnectable={false}
              elementsSelectable={false}
              zoomOnDoubleClick={false}
              className="bg-[radial-gradient(circle_at_top,rgba(10,132,255,0.08),transparent_35%),linear-gradient(180deg,rgba(255,255,255,0.02),transparent_65%)]"
            >
              <Background
                variant={BackgroundVariant.Dots}
                gap={20}
                size={1}
                color="rgba(148, 163, 184, 0.18)"
              />
              <Controls showInteractive={false} />
            </ReactFlow>
          </CardContent>
        </Card>

        <CommandMapInspector selectedNode={selectedNode} />
      </div>
    </div>
  )
}

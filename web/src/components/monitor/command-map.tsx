import { Background, BackgroundVariant, Controls, ReactFlow, useReactFlow } from '@xyflow/react'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { CommandMapModel } from '@/lib/command-map/types'
import { CommandMapToolbar } from './command-map-toolbar'
import { CommandMapInspector } from './command-map-inspector'
import { useCommandMapState } from './use-command-map-state'
import { commandMapNodeTypes } from './node-renderers'
import { useNodeLayout } from './use-node-layout'
import { useIsMobile } from '@/hooks/use-mobile'
import { RunRecipeDialog } from './run-recipe-dialog'
import { EditTriggerDialog } from './edit-trigger-dialog'
import { ThreadDetailsDialog } from './thread-details-dialog'
import { EventLogDrawer } from './event-log-drawer'

interface CommandMapProps {
  model: CommandMapModel
  focusNodeId?: string | null
  focusMode?: 'focus' | 'select'
}

type CommandMapOverlay =
  | { kind: 'recipe'; skillName: string }
  | { kind: 'trigger'; scheduleId: string }
  | { kind: 'thread'; sessionId: string }
  | { kind: 'events'; title: string; sessionId?: string | null; eventType?: string | null }

function CommandMapViewportFocus({ focusNodeId }: { focusNodeId: string | null }) {
  const reactFlow = useReactFlow()
  const lastFocusedNodeId = useRef<string | null>(null)

  useEffect(() => {
    if (!focusNodeId) {
      lastFocusedNodeId.current = null
      return
    }

    if (!reactFlow.viewportInitialized) return
    if (lastFocusedNodeId.current === focusNodeId) return

    const target = reactFlow.getNode(focusNodeId)
    if (!target) return

    lastFocusedNodeId.current = focusNodeId
    void reactFlow.fitView({
      nodes: [{ id: focusNodeId }],
      padding: 0.35,
      duration: 250,
    })
  }, [focusNodeId, reactFlow])

  return null
}

export function CommandMap({ model, focusNodeId = null, focusMode = 'select' }: CommandMapProps) {
  const isMobile = useIsMobile()
  const [activeOverlay, setActiveOverlay] = useState<CommandMapOverlay | null>(null)
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

  const handleSelectNode = useCallback((nodeId: string | null) => {
    setActiveOverlay(null)
    selectNode(nodeId)
  }, [selectNode])

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
    onSelectNode: handleSelectNode,
  })

  const inspector = (
    <CommandMapInspector
      selectedNode={selectedNode}
      onOpenRecipeDetails={(skillName) => setActiveOverlay({ kind: 'recipe', skillName })}
      onOpenTriggerDetails={(scheduleId) => setActiveOverlay({ kind: 'trigger', scheduleId })}
      onOpenThreadDetails={(sessionId) => setActiveOverlay({ kind: 'thread', sessionId })}
      onOpenEventLog={(context) => setActiveOverlay({
        kind: 'events',
        title: context.title,
        sessionId: context.sessionId ?? null,
        eventType: context.eventType ?? null,
      })}
    />
  )

  function handleReset() {
    setActiveOverlay(null)
    resetView()
    resetLayout()
  }

  function handleMobileSheetChange(open: boolean) {
    if (!open) {
      handleSelectNode(null)
    }
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
              <CommandMapViewportFocus focusNodeId={focusNodeId} />
            </ReactFlow>
          </CardContent>
        </Card>

        {isMobile ? (
          <Sheet open={selectedNode !== null && activeOverlay === null} onOpenChange={handleMobileSheetChange}>
            <SheetContent side="bottom" className="h-[min(80vh,48rem)] overflow-auto">
              <SheetHeader className="sr-only">
                <SheetTitle>Command map selection</SheetTitle>
                <SheetDescription>Shows details for the selected command map node.</SheetDescription>
              </SheetHeader>
              <div className="p-4">
                {inspector}
              </div>
            </SheetContent>
          </Sheet>
        ) : (
          inspector
        )}
      </div>

      <RunRecipeDialog
        open={activeOverlay?.kind === 'recipe'}
        onOpenChange={(open) => !open && setActiveOverlay(null)}
        skillName={activeOverlay?.kind === 'recipe' ? activeOverlay.skillName : null}
      />
      <EditTriggerDialog
        open={activeOverlay?.kind === 'trigger'}
        onOpenChange={(open) => !open && setActiveOverlay(null)}
        scheduleId={activeOverlay?.kind === 'trigger' ? activeOverlay.scheduleId : null}
      />
      <ThreadDetailsDialog
        open={activeOverlay?.kind === 'thread'}
        onOpenChange={(open) => !open && setActiveOverlay(null)}
        sessionId={activeOverlay?.kind === 'thread' ? activeOverlay.sessionId : null}
      />
      <EventLogDrawer
        open={activeOverlay?.kind === 'events'}
        onOpenChange={(open) => !open && setActiveOverlay(null)}
        title={activeOverlay?.kind === 'events' ? activeOverlay.title : 'Event log'}
        sessionId={activeOverlay?.kind === 'events' ? activeOverlay.sessionId : null}
        eventType={activeOverlay?.kind === 'events' ? activeOverlay.eventType : null}
      />
    </div>
  )
}

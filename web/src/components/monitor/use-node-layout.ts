import type { Edge } from '@xyflow/react'
import { MarkerType, useEdgesState, useNodesState } from '@xyflow/react'
import { useCallback, useEffect, useMemo, useState } from 'react'
import { applyCommandMapLayout } from '@/lib/command-map/layout'
import type { CommandMapEdge, CommandMapNode } from '@/lib/command-map/types'
import {
  clearCommandMapPinnedPositions,
  loadCommandMapPinnedPositions,
  saveCommandMapPinnedPositions,
} from '@/lib/command-map/storage'
import type { CommandMapFlowNode } from './node-renderers'

interface UseNodeLayoutOptions {
  nodes: CommandMapNode[]
  allNodes: CommandMapNode[]
  edges: CommandMapEdge[]
  selectedNodeId: string | null
  isFocused: boolean
  onSelectNode: (nodeId: string) => void
}

type CommandMapFlowEdge = Edge

function edgeStyle(edge: CommandMapEdge, isDimmed: boolean) {
  if (edge.kind === 'alert') {
    return {
      stroke: isDimmed ? 'rgba(248, 113, 113, 0.28)' : 'rgba(248, 113, 113, 0.75)',
      strokeWidth: 1.5,
    }
  }

  return {
    stroke: isDimmed ? 'rgba(148, 163, 184, 0.18)' : 'rgba(148, 163, 184, 0.42)',
    strokeWidth: 1.25,
  }
}

export function useNodeLayout({
  nodes,
  allNodes,
  edges,
  selectedNodeId,
  isFocused,
  onSelectNode,
}: UseNodeLayoutOptions) {
  const [pinnedPositions, setPinnedPositions] = useState(() => loadCommandMapPinnedPositions())
  const knownNodeIds = useMemo(
    () => new Set(allNodes.map(node => node.id)),
    [allNodes],
  )

  const connectedNodeIds = useMemo(() => {
    if (!selectedNodeId || !isFocused) return new Set<string>()

    const connected = new Set<string>([selectedNodeId])
    for (const edge of edges) {
      if (edge.source === selectedNodeId) connected.add(edge.target)
      if (edge.target === selectedNodeId) connected.add(edge.source)
    }

    return connected
  }, [edges, isFocused, selectedNodeId])

  const positionedNodes = useMemo(
    () => applyCommandMapLayout(nodes, pinnedPositions),
    [nodes, pinnedPositions],
  )

  const togglePinned = useCallback((nodeId: string) => {
    setPinnedPositions(current => {
      if (current[nodeId]) {
        const next = { ...current }
        delete next[nodeId]
        return next
      }

      const node = positionedNodes.find(candidate => candidate.id === nodeId)
      if (!node) return current

      return {
        ...current,
        [nodeId]: node.position,
      }
    })
  }, [positionedNodes])

  const nextFlowNodes = useMemo<CommandMapFlowNode[]>(() => positionedNodes.map(node => ({
    id: node.id,
    type: 'commandMapNode',
    position: node.position,
    draggable: node.kind !== 'eve',
    selectable: false,
    data: {
      node,
      isSelected: node.id === selectedNodeId,
      isDimmed: isFocused && selectedNodeId !== null && !connectedNodeIds.has(node.id),
      onSelectNode,
      onTogglePinned: togglePinned,
    },
  })), [connectedNodeIds, isFocused, onSelectNode, positionedNodes, selectedNodeId, togglePinned])

  const nextFlowEdges = useMemo<CommandMapFlowEdge[]>(() => edges.map(edge => {
    const isDimmed = isFocused && selectedNodeId !== null && edge.source !== selectedNodeId && edge.target !== selectedNodeId
    return {
      id: edge.id,
      source: edge.source,
      target: edge.target,
      type: 'smoothstep',
      animated: edge.kind === 'alert',
      selectable: false,
      style: edgeStyle(edge, isDimmed),
      markerEnd: {
        type: MarkerType.ArrowClosed,
        color: edge.kind === 'alert' ? 'rgba(248, 113, 113, 0.75)' : 'rgba(148, 163, 184, 0.42)',
      },
    }
  }), [edges, isFocused, selectedNodeId])

  const [flowNodes, setFlowNodes, onNodesChange] = useNodesState(nextFlowNodes)
  const [flowEdges, setFlowEdges, onEdgesChange] = useEdgesState(nextFlowEdges)

  useEffect(() => {
    setFlowNodes(nextFlowNodes)
  }, [nextFlowNodes, setFlowNodes])

  useEffect(() => {
    setFlowEdges(nextFlowEdges)
  }, [nextFlowEdges, setFlowEdges])

  useEffect(() => {
    setPinnedPositions(current => {
      const next = Object.fromEntries(
        Object.entries(current).filter(([nodeId]) => knownNodeIds.has(nodeId)),
      )

      return Object.keys(next).length === Object.keys(current).length ? current : next
    })
  }, [knownNodeIds])

  useEffect(() => {
    if (Object.keys(pinnedPositions).length === 0) {
      clearCommandMapPinnedPositions()
      return
    }

    saveCommandMapPinnedPositions(pinnedPositions)
  }, [pinnedPositions])

  const onNodeDragStop = useCallback((_: unknown, node: CommandMapFlowNode) => {
    setPinnedPositions(current => ({
      ...current,
      [node.id]: node.position,
    }))
  }, [])

  const resetLayout = useCallback(() => {
    setPinnedPositions({})
  }, [])

  return {
    flowNodes,
    flowEdges,
    onNodesChange,
    onEdgesChange,
    onNodeDragStop,
    resetLayout,
    hasPinnedNodes: Object.keys(pinnedPositions).some(nodeId => knownNodeIds.has(nodeId)),
  }
}

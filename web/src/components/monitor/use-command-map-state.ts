import { useEffect, useMemo, useState } from 'react'
import type { CommandMapNode, CommandMapNodeLayer } from '@/lib/command-map/types'

const DEFAULT_LAYERS: CommandMapNodeLayer[] = ['core', 'execution', 'trigger', 'system', 'alert']

export function useCommandMapState(nodes: CommandMapNode[]) {
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
  const [isDecluttered, setIsDecluttered] = useState(false)
  const [isFocused, setIsFocused] = useState(false)
  const [visibleLayers, setVisibleLayers] = useState<Record<CommandMapNodeLayer, boolean>>({
    core: true,
    execution: true,
    trigger: true,
    system: true,
    alert: true,
  })

  function toggleLayer(layer: CommandMapNodeLayer) {
    setVisibleLayers(current => ({
      ...current,
      [layer]: !current[layer],
    }))
  }

  function toggleDeclutter() {
    setIsDecluttered(current => !current)
  }

  const layerVisibleNodes = useMemo(
    () => nodes.filter(node => visibleLayers[node.layer]),
    [nodes, visibleLayers],
  )

  const visibleNodes = useMemo(
    () => layerVisibleNodes.filter(node => {
      if (!isDecluttered) return true
      if (node.id === selectedNodeId) return true
      return node.kind === 'eve' || node.kind === 'thread' || node.kind === 'trigger'
    }),
    [isDecluttered, layerVisibleNodes, selectedNodeId],
  )

  const selectedNode = visibleNodes.find(node => node.id === selectedNodeId) ?? null

  useEffect(() => {
    if (!selectedNodeId) {
      if (isFocused) setIsFocused(false)
      return
    }

    const selectionStillVisible = layerVisibleNodes.some(node => node.id === selectedNodeId)
    if (!selectionStillVisible) {
      setSelectedNodeId(null)
      setIsFocused(false)
    }
  }, [isFocused, layerVisibleNodes, selectedNodeId])

  function toggleFocus() {
    if (!selectedNode) return
    setIsFocused(current => !current)
  }

  function resetView() {
    setSelectedNodeId(null)
    setIsDecluttered(false)
    setIsFocused(false)
    setVisibleLayers(
      DEFAULT_LAYERS.reduce(
        (acc, layer) => {
          acc[layer] = true
          return acc
        },
        {} as Record<CommandMapNodeLayer, boolean>,
      ),
    )
  }

  return {
    selectedNodeId,
    selectedNode,
    isDecluttered,
    isFocused,
    visibleLayers,
    visibleNodes,
    selectNode: setSelectedNodeId,
    toggleLayer,
    toggleDeclutter,
    toggleFocus,
    resetView,
  }
}

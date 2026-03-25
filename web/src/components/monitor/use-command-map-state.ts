import { useState } from 'react'
import type { CommandMapNode, CommandMapNodeLayer } from '@/lib/command-map/types'

const DEFAULT_LAYERS: CommandMapNodeLayer[] = ['core', 'execution', 'trigger', 'system', 'alert']

export function useCommandMapState(nodes: CommandMapNode[]) {
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null)
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

  function resetView() {
    setSelectedNodeId(null)
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

  const selectedNode = nodes.find(node => node.id === selectedNodeId) ?? null
  const visibleNodes = nodes.filter(node => visibleLayers[node.layer])

  return {
    selectedNodeId,
    selectedNode,
    visibleLayers,
    visibleNodes,
    selectNode: setSelectedNodeId,
    toggleLayer,
    resetView,
  }
}

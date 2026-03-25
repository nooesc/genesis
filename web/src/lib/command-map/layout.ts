import type { CommandMapNode, CommandMapNodeLayer, CommandMapPoint } from './types'

export const COMMAND_MAP_RINGS = {
  core: 0,
  execution: 1,
  trigger: 2,
  recipe: 2,
  system: 3,
  alert: 4,
} as const

const RING_RADII: Record<number, number> = {
  [COMMAND_MAP_RINGS.core]: 0,
  [COMMAND_MAP_RINGS.execution]: 190,
  [COMMAND_MAP_RINGS.trigger]: 300,
  [COMMAND_MAP_RINGS.system]: 410,
  [COMMAND_MAP_RINGS.alert]: 510,
}

export function ringRadius(ring: number): number {
  return RING_RADII[ring] ?? 190 + ring * 110
}

export function orderForLayout(nodes: CommandMapNode[]): CommandMapNode[] {
  const layerOrder: Record<CommandMapNodeLayer, number> = {
    core: 0,
    execution: 1,
    trigger: 2,
    recipe: 3,
    system: 4,
    alert: 5,
  }

  return [...nodes].sort((a, b) => {
    if (a.ring !== b.ring) return a.ring - b.ring
    if (a.layer !== b.layer) return layerOrder[a.layer] - layerOrder[b.layer]
    const labelCompare = a.label.localeCompare(b.label)
    if (labelCompare !== 0) return labelCompare
    return a.id.localeCompare(b.id)
  })
}

export function defaultAutoPlacement(nodes: CommandMapNode[]): CommandMapNode[] {
  const ordered = orderForLayout(nodes)
  const grouped = new Map<number, CommandMapNode[]>()

  for (const node of ordered) {
    const group = grouped.get(node.ring) ?? []
    group.push(node)
    grouped.set(node.ring, group)
  }

  return ordered.map((node) => {
    const group = grouped.get(node.ring) ?? []
    const index = group.findIndex(candidate => candidate.id === node.id)
    const total = group.length

    if (node.ring === COMMAND_MAP_RINGS.core) {
      return { ...node, position: { x: 0, y: 0 } }
    }

    const radius = ringRadius(node.ring)
    const angle = ((index >= 0 ? index : 0) / Math.max(total, 1)) * Math.PI * 2 - Math.PI / 2
    return {
      ...node,
      position: {
        x: Math.cos(angle) * radius,
        y: Math.sin(angle) * radius,
      },
    }
  })
}

export function mergePinnedPositions(
  nodes: CommandMapNode[],
  pinnedPositions: Record<string, CommandMapPoint | undefined> = {},
): CommandMapNode[] {
  return nodes.map((node) => {
    const pinned = pinnedPositions[node.id]
    if (!pinned) return node
    return {
      ...node,
      pinned: true,
      position: pinned,
    }
  })
}

export function applyCommandMapLayout(
  nodes: CommandMapNode[],
  pinnedPositions: Record<string, CommandMapPoint | undefined> = {},
): CommandMapNode[] {
  return mergePinnedPositions(defaultAutoPlacement(nodes), pinnedPositions)
}

import type { CommandMapModel, CommandMapNode, CommandMapNodeKind, CommandMapNodeLayer, CommandMapNodeStatus } from './types'

export interface CommandMapSearchEntry {
  nodeId: string
  label: string
  subtitle: string
  kind: CommandMapNodeKind
  layer: CommandMapNodeLayer
  status: CommandMapNodeStatus | 'unknown'
  keywords: string[]
  searchText: string
}

export interface CommandMapSearchFilters {
  query?: string
  kind?: CommandMapNodeKind
  layer?: CommandMapNodeLayer
  status?: CommandMapNodeStatus | 'unknown'
}

function buildKeywords(node: CommandMapNode): string[] {
  return [
    node.label,
    node.subtitle ?? '',
    node.kind,
    node.layer,
    node.status ?? 'unknown',
  ]
    .join(' ')
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
}

export function buildCommandMapSearchIndex(model: CommandMapModel): CommandMapSearchEntry[] {
  return model.nodes.map(node => {
    const keywords = buildKeywords(node)

    return {
      nodeId: node.id,
      label: node.label,
      subtitle: node.subtitle ?? '',
      kind: node.kind,
      layer: node.layer,
      status: node.status ?? 'unknown',
      keywords,
      searchText: keywords.join(' '),
    }
  })
}

export function filterCommandMapSearchIndex(
  index: CommandMapSearchEntry[],
  filters: CommandMapSearchFilters = {},
): CommandMapSearchEntry[] {
  const query = filters.query?.trim().toLowerCase()

  return index.filter(entry => {
    if (filters.kind && entry.kind !== filters.kind) return false
    if (filters.layer && entry.layer !== filters.layer) return false
    if (filters.status && entry.status !== filters.status) return false
    if (query && !entry.searchText.includes(query)) return false
    return true
  })
}

export function buildCommandMapJumpTarget(nodeId: string) {
  return {
    to: '/monitor' as const,
    search: {
      focusNodeId: nodeId,
      focusMode: 'focus' as const,
    },
  }
}

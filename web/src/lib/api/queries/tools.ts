import { useQuery } from '@tanstack/react-query'
import { api } from '../client'
import type { ToolInfo } from '../types'

interface ToolsResponse {
  builtin_tools: ToolInfo[]
  builtin_count: number
  mcp_tools: ToolInfo[]
  mcp_count: number
  total: number
}

export function useTools() {
  return useQuery({
    queryKey: ['tools'],
    queryFn: async () => {
      const res = await api.get<ToolsResponse>('/tools')
      // Merge builtin and MCP tools, tagging each with source
      const builtin = res.builtin_tools.map((t) => ({ ...t, source: t.source ?? 'builtin' }))
      const mcp = res.mcp_tools.map((t) => ({ ...t, source: t.source ?? 'mcp' }))
      return [...builtin, ...mcp] as ToolInfo[]
    },
  })
}

import type { AuditEntry, HealthResponse, InsightsData, Schedule, SessionSummary } from '@/lib/api/types'

export type CommandMapNodeKind = 'eve' | 'thread' | 'trigger' | 'system' | 'alert'

export type CommandMapNodeLayer = 'core' | 'execution' | 'trigger' | 'system' | 'alert'

export type CommandMapNodeStatus = 'ok' | 'warning' | 'error' | 'idle'

export interface CommandMapPoint {
  x: number
  y: number
}

export interface CommandMapNode {
  id: string
  kind: CommandMapNodeKind
  layer: CommandMapNodeLayer
  ring: number
  label: string
  subtitle?: string
  status?: CommandMapNodeStatus
  pinned?: boolean
  position: CommandMapPoint
  data?: Record<string, string | number | boolean | null>
}

export interface CommandMapEdge {
  id: string
  source: string
  target: string
  label?: string
  kind?: 'context' | 'flow' | 'alert'
}

export interface CommandMapModel {
  nodes: CommandMapNode[]
  edges: CommandMapEdge[]
}

export interface CommandMapProjectionInput {
  health: HealthResponse | null
  sessions: readonly SessionSummary[]
  schedules: readonly Schedule[]
  audit: readonly AuditEntry[]
  insights: InsightsData | null
}

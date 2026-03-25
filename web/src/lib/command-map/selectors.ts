import type { CommandMapEdge, CommandMapModel, CommandMapNode, CommandMapProjectionInput } from './types'
import { COMMAND_MAP_RINGS, applyCommandMapLayout, ringLayer } from './layout'

function sortByTimeThenId<T extends { id: string; created_at?: string; updated_at?: string }>(
  items: readonly T[],
): T[] {
  return [...items].sort((a, b) => {
    const aTime = Date.parse(a.updated_at ?? a.created_at ?? '')
    const bTime = Date.parse(b.updated_at ?? b.created_at ?? '')
    if (Number.isFinite(aTime) && Number.isFinite(bTime) && aTime !== bTime) {
      return bTime - aTime
    }
    if (a.id !== b.id) return a.id.localeCompare(b.id)
    return 0
  })
}

function formatStatus(status: string): 'ok' | 'warning' | 'error' {
  if (status === 'ok' || status === 'healthy') return 'ok'
  if (status === 'degraded' || status === 'warn' || status === 'warning') return 'warning'
  return 'error'
}

function safeJsonParse(value: string | null): Record<string, unknown> | null {
  if (!value) return null
  try {
    const parsed = JSON.parse(value)
    return parsed && typeof parsed === 'object' ? parsed as Record<string, unknown> : null
  } catch {
    return null
  }
}

function makeNodeId(prefix: string, id: string): string {
  return `${prefix}-${id}`
}

export function buildEveNode(health: CommandMapProjectionInput['health']): CommandMapNode {
  if (!health) {
    return {
      id: 'eve',
      kind: 'eve',
      layer: ringLayer(COMMAND_MAP_RINGS.core),
      ring: COMMAND_MAP_RINGS.core,
      label: 'Eve',
      subtitle: 'offline · gateway unavailable',
      status: 'error',
      position: { x: 0, y: 0 },
      data: {
        model: null,
        uptime_seconds: null,
        mcp_servers: null,
        active_schedules: null,
        total_sessions: null,
        total_tools: null,
      },
    }
  }

  return {
    id: 'eve',
    kind: 'eve',
    layer: ringLayer(COMMAND_MAP_RINGS.core),
    ring: COMMAND_MAP_RINGS.core,
    label: 'Eve',
    subtitle: `${health.status} · ${health.version}`,
    status: formatStatus(health.status),
    position: { x: 0, y: 0 },
    data: {
      model: health.model,
      uptime_seconds: health.uptime_seconds,
      mcp_servers: health.mcp_servers,
      active_schedules: health.active_schedules,
      total_sessions: health.total_sessions,
      total_tools: health.total_tools,
    },
  }
}

export function buildSessionNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  return sortByTimeThenId(input.sessions).map((session, index) => ({
    id: makeNodeId('session', session.id),
    kind: 'thread',
    layer: ringLayer(COMMAND_MAP_RINGS.execution),
    ring: COMMAND_MAP_RINGS.execution,
    label: session.title?.trim() || session.id,
    subtitle: [session.platform, `${session.total_input_tokens + session.total_output_tokens} tok`].join(' · '),
    status: session.updated_at ? 'ok' : 'idle',
    position: { x: 0, y: 0 },
    data: {
      session_id: session.id,
      platform: session.platform,
      total_input_tokens: session.total_input_tokens,
      total_output_tokens: session.total_output_tokens,
      index,
    },
  }))
}

export function buildScheduleNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  return sortByTimeThenId(input.schedules).map(schedule => ({
    id: makeNodeId('schedule', schedule.id),
    kind: 'trigger',
    layer: ringLayer(COMMAND_MAP_RINGS.trigger),
    ring: COMMAND_MAP_RINGS.trigger,
    label: schedule.id,
    subtitle: `${schedule.cron_expression} · ${schedule.destination}`,
    status: schedule.enabled ? 'ok' : 'idle',
    position: { x: 0, y: 0 },
    data: {
      schedule_id: schedule.id,
      enabled: schedule.enabled,
      prompt: schedule.prompt,
      last_run_at: schedule.last_run_at,
    },
  }))
}

export function buildSystemNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  const nodes: CommandMapNode[] = []

  if (input.health) {
    nodes.push(
      {
        id: makeNodeId('system', 'model'),
        kind: 'system',
        layer: ringLayer(COMMAND_MAP_RINGS.system),
        ring: COMMAND_MAP_RINGS.system,
        label: 'Model',
        subtitle: input.health.model,
        status: 'ok',
        position: { x: 0, y: 0 },
        data: {
          model: input.health.model,
        },
      },
      {
        id: makeNodeId('system', 'mcp'),
        kind: 'system',
        layer: ringLayer(COMMAND_MAP_RINGS.system),
        ring: COMMAND_MAP_RINGS.system,
        label: 'MCP',
        subtitle: `${input.health.mcp_servers} servers`,
        status: input.health.mcp_servers > 0 ? 'ok' : 'warning',
        position: { x: 0, y: 0 },
        data: {
          mcp_servers: input.health.mcp_servers,
        },
      },
    )
  }

  if (input.insights) {
    for (const [platform, count] of input.insights.platform_breakdown) {
      nodes.push({
        id: makeNodeId('platform', platform),
        kind: 'system',
        layer: ringLayer(COMMAND_MAP_RINGS.system),
        ring: COMMAND_MAP_RINGS.system,
        label: platform,
        subtitle: `${count} sessions`,
        status: count > 0 ? 'ok' : 'idle',
        position: { x: 0, y: 0 },
        data: {
          platform,
          count,
        },
      })
    }
  }

  return nodes.sort((a, b) => a.id.localeCompare(b.id))
}

function isAlertEntry(entry: CommandMapProjectionInput['audit'][number]): boolean {
  if (entry.event_type === 'stuck_loop') return true
  if (entry.event_type.includes('error')) return true
  if (entry.event_type === 'tool_call_end') {
    const details = safeJsonParse(entry.details)
    if (details && details.success === false) return true
  }
  return false
}

export function buildAlertNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  return sortByTimeThenId(input.audit)
    .filter(isAlertEntry)
    .map(entry => {
      const details = safeJsonParse(entry.details)
      const tool = typeof details?.tool === 'string' ? details.tool : null
      const failureCount = typeof details?.failure_count === 'number' ? details.failure_count : null
      const success = typeof details?.success === 'boolean' ? details.success : null

      const subtitleParts = [
        entry.session_id ? `session ${entry.session_id}` : null,
        tool,
        failureCount !== null ? `${failureCount} failures` : null,
        success === false ? 'failed' : null,
      ].filter(Boolean)

      return {
        id: makeNodeId('alert', entry.id),
        kind: 'alert',
        layer: ringLayer(COMMAND_MAP_RINGS.alert),
        ring: COMMAND_MAP_RINGS.alert,
        label: entry.event_type,
        subtitle: subtitleParts.join(' · ') || entry.created_at,
        status: 'error',
        position: { x: 0, y: 0 },
        data: {
          event_type: entry.event_type,
          session_id: entry.session_id,
          details: entry.details,
        },
      }
    })
}

export function buildCommandMapModel(input: CommandMapProjectionInput): CommandMapModel {
  const nodes = applyCommandMapLayout([
    buildEveNode(input.health),
    ...buildSessionNodes(input),
    ...buildScheduleNodes(input),
    ...buildSystemNodes(input),
    ...buildAlertNodes(input),
  ])

  const edges: CommandMapEdge[] = []
  const byId = new Map(nodes.map(node => [node.id, node]))

  for (const node of nodes) {
    if (node.id === 'eve') continue
    if (node.kind === 'alert' && typeof node.data?.session_id === 'string') {
      const source = makeNodeId('session', node.data.session_id)
      if (byId.has(source)) {
        edges.push({
          id: `${source}->${node.id}`,
          source,
          target: node.id,
          kind: 'alert',
        })
        continue
      }
    }

    edges.push({
      id: `eve->${node.id}`,
      source: 'eve',
      target: node.id,
      kind: node.kind === 'alert' ? 'alert' : 'context',
    })
  }

  return { nodes, edges }
}

import type { CommandMapEdge, CommandMapModel, CommandMapNode, CommandMapProjectionInput } from './types'
import { COMMAND_MAP_RINGS, applyCommandMapLayout } from './layout'

function sortByTimeThenKey<T extends { created_at?: string; updated_at?: string }>(
  items: readonly T[],
  getKey: (item: T) => string,
): T[] {
  return [...items].sort((a, b) => {
    const aTime = Date.parse(a.updated_at ?? a.created_at ?? '')
    const bTime = Date.parse(b.updated_at ?? b.created_at ?? '')
    if (Number.isFinite(aTime) && Number.isFinite(bTime) && aTime !== bTime) {
      return bTime - aTime
    }

    const aKey = getKey(a)
    const bKey = getKey(b)
    if (aKey !== bKey) return aKey.localeCompare(bKey)
    return 0
  })
}

function sortByTimeThenId<T extends { id: string; created_at?: string; updated_at?: string }>(
  items: readonly T[],
): T[] {
  return sortByTimeThenKey(items, item => item.id)
}

function sortByTimeThenName<T extends { name: string; created_at?: string; updated_at?: string }>(
  items: readonly T[],
): T[] {
  return sortByTimeThenKey(items, item => item.name)
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

function buildNodeBase(
  kind: CommandMapNode['kind'],
  id: string,
  label: string,
  subtitle?: string,
  status?: CommandMapNode['status'],
): Omit<CommandMapNode, 'data'> {
  const layer: CommandMapNode['layer'] = kind === 'eve'
    ? 'core'
    : kind === 'thread'
      ? 'execution'
      : kind === 'trigger'
        ? 'trigger'
        : kind === 'recipe'
          ? 'recipe'
          : kind === 'system'
            ? 'system'
            : 'alert'

  const ring = kind === 'eve'
    ? COMMAND_MAP_RINGS.core
    : kind === 'thread'
      ? COMMAND_MAP_RINGS.execution
      : kind === 'trigger'
        ? COMMAND_MAP_RINGS.trigger
        : kind === 'recipe'
          ? COMMAND_MAP_RINGS.recipe
          : kind === 'system'
            ? COMMAND_MAP_RINGS.system
            : COMMAND_MAP_RINGS.alert

  return {
    id,
    kind,
    layer,
    ring,
    label,
    subtitle,
    status,
    position: { x: 0, y: 0 },
  }
}

export function buildEveNode(health: CommandMapProjectionInput['health']): CommandMapNode {
  if (!health) {
    return {
      ...buildNodeBase('eve', 'eve', 'Eve', 'offline · gateway unavailable', 'error'),
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
    ...buildNodeBase('eve', 'eve', 'Eve', `${health.status} · ${health.version}`, formatStatus(health.status)),
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
    ...buildNodeBase(
      'thread',
      makeNodeId('session', session.id),
      session.title?.trim() || session.id,
      [session.platform, `${session.total_input_tokens + session.total_output_tokens} tok`].join(' · '),
      session.updated_at ? 'ok' : 'idle',
    ),
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
    ...buildNodeBase(
      'trigger',
      makeNodeId('schedule', schedule.id),
      schedule.id,
      `${schedule.cron_expression} · ${schedule.destination}`,
      schedule.enabled ? 'ok' : 'idle',
    ),
    data: {
      schedule_id: schedule.id,
      enabled: schedule.enabled,
      prompt: schedule.prompt,
      last_run_at: schedule.last_run_at,
    },
  }))
}

export function buildRecipeNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  return sortByTimeThenName(input.skills).map(skill => {
    const subtitleParts = [
      skill.description.trim() || null,
      skill.tags.length > 0 ? skill.tags.join(', ') : null,
    ].filter(Boolean)

    return {
      ...buildNodeBase(
        'recipe',
        makeNodeId('skill', skill.name),
        skill.name,
        subtitleParts.join(' · ') || 'saved recipe',
        skill.instructions.trim().length > 0 || skill.description.trim().length > 0 ? 'ok' : 'idle',
      ),
      data: {
        skill_name: skill.name,
        tag_count: skill.tags.length,
        instruction_length: skill.instructions.length,
      },
    }
  })
}

export function buildSystemNodes(input: CommandMapProjectionInput): CommandMapNode[] {
  const nodes: CommandMapNode[] = []

  if (input.health) {
    nodes.push(
      {
        ...buildNodeBase('system', makeNodeId('system', 'model'), 'Model', input.health.model, 'ok'),
        data: {
          model: input.health.model,
        },
      },
      {
        ...buildNodeBase(
          'system',
          makeNodeId('system', 'mcp'),
          'MCP',
          `${input.health.mcp_servers} servers`,
          input.health.mcp_servers > 0 ? 'ok' : 'warning',
        ),
        data: {
          mcp_servers: input.health.mcp_servers,
        },
      },
    )
  }

  if (input.insights) {
    for (const [platform, count] of input.insights.platform_breakdown) {
      nodes.push({
        ...buildNodeBase(
          'system',
          makeNodeId('platform', platform),
          platform,
          `${count} sessions`,
          count > 0 ? 'ok' : 'idle',
        ),
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
        ...buildNodeBase(
          'alert',
          makeNodeId('alert', entry.id),
          entry.event_type,
          subtitleParts.join(' · ') || entry.created_at,
          'error',
        ),
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
    ...buildRecipeNodes(input),
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

import { describe, expect, it } from 'vitest'
import { buildCommandMapModel } from './selectors'
import { COMMAND_MAP_RINGS } from './layout'
import type { CommandMapProjectionInput } from './types'

describe('buildCommandMapModel', () => {
  const baseInput: CommandMapProjectionInput = {
    health: {
      status: 'ok',
      version: '1.0.0',
      uptime_seconds: 60,
      model: 'gpt',
      mcp_servers: 1,
      active_schedules: 1,
      total_sessions: 2,
      total_tools: 3,
    },
    sessions: [
      {
        id: 's1',
        title: 'Alpha',
        platform: 'api',
        total_input_tokens: 10,
        total_output_tokens: 5,
        parent_session_id: null,
        created_at: '2026-03-24T00:00:00Z',
        updated_at: '2026-03-24T00:00:00Z',
      },
    ],
    skills: [
      {
        name: 'deploy-service',
        description: 'Run the standard deployment recipe',
        content: 'Deploy the service with the approved steps.',
        tags: ['ops', 'release'],
        created_at: '2026-03-24T00:00:00Z',
        updated_at: '2026-03-24T00:00:00Z',
      },
    ],
    schedules: [
      {
        id: 'nightly',
        cron_expression: '0 0 * * *',
        destination: 'api',
        prompt: 'Run nightly',
        enabled: true,
        created_at: '2026-03-24T00:00:00Z',
        last_run_at: null,
      },
    ],
    audit: [],
    insights: null,
  } as const

  it('creates Eve core node', () => {
    const model = buildCommandMapModel(baseInput)

    const eve = model.nodes.find(node => node.id === 'eve')
    expect(eve).toBeDefined()
    expect(eve?.kind).toBe('eve')
    expect(eve?.ring).toBe(COMMAND_MAP_RINGS.core)
  })

  it('derives execution nodes from sessions', () => {
    const model = buildCommandMapModel(baseInput)

    const thread = model.nodes.find(node => node.id === 'session-s1')
    expect(thread).toBeDefined()
    expect(thread?.kind).toBe('thread')
    expect(thread?.ring).toBe(COMMAND_MAP_RINGS.execution)
  })

  it('derives trigger nodes from schedules', () => {
    const model = buildCommandMapModel(baseInput)

    const trigger = model.nodes.find(node => node.id === 'schedule-nightly')
    expect(trigger).toBeDefined()
    expect(trigger?.kind).toBe('trigger')
    expect(trigger?.ring).toBe(COMMAND_MAP_RINGS.trigger)
  })

  it('derives recipe nodes from skills', () => {
    const model = buildCommandMapModel(baseInput)

    const recipe = model.nodes.find(node => node.id === 'skill-deploy-service')
    expect(recipe).toBeDefined()
    expect(recipe?.kind).toBe('recipe')
    expect(recipe?.layer).toBe('recipe')
    expect(recipe?.ring).toBe(COMMAND_MAP_RINGS.recipe)
  })

  it('derives alert nodes from failed or degraded audit entries', () => {
    const model = buildCommandMapModel({
      ...baseInput,
      audit: [
        {
          id: 'a1',
          event_type: 'stuck_loop',
          session_id: 's1',
          details: '{"tool":"shell","failure_count":3}',
          created_at: '2026-03-24T00:00:00Z',
        },
        {
          id: 'a2',
          event_type: 'tool_call_end',
          session_id: 's1',
          details: '{"tool":"shell","success":false,"duration_ms":24}',
          created_at: '2026-03-24T00:01:00Z',
        },
      ],
    })

    const alerts = model.nodes.filter(node => node.kind === 'alert')
    expect(alerts.length).toBeGreaterThan(0)
    expect(alerts.every(node => node.ring === COMMAND_MAP_RINGS.alert)).toBe(true)
  })

  it('keeps ring assignments stable across repeated projections', () => {
    const first = buildCommandMapModel(baseInput)
    const second = buildCommandMapModel(baseInput)

    const firstRings = first.nodes.map(node => [node.id, node.ring] as const)
    const secondRings = second.nodes.map(node => [node.id, node.ring] as const)

    expect(secondRings).toEqual(firstRings)
  })

  it('projects a degraded Eve node when health is unavailable', () => {
    const model = buildCommandMapModel({
      ...baseInput,
      health: null,
    })

    const eve = model.nodes.find(node => node.id === 'eve')
    expect(eve).toBeDefined()
    expect(eve?.status).toBe('error')
    expect(eve?.subtitle).toMatch(/offline/i)
  })
})

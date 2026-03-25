import { describe, expect, it } from 'vitest'
import { buildCommandMapModel } from './selectors'

describe('buildCommandMapModel', () => {
  it('creates Eve plus derived execution and trigger nodes', () => {
    const model = buildCommandMapModel({
      health: { status: 'ok', version: '1.0.0', uptime_seconds: 60, model: 'gpt', mcp_servers: 1, active_schedules: 1, total_sessions: 2, total_tools: 3 },
      sessions: [{ id: 's1', title: 'Alpha', platform: 'api', total_input_tokens: 10, total_output_tokens: 5, parent_session_id: null, created_at: '2026-03-24T00:00:00Z', updated_at: '2026-03-24T00:00:00Z' }],
      schedules: [{ id: 'nightly', cron_expression: '0 0 * * *', destination: 'api', prompt: 'Run nightly', enabled: true, created_at: '2026-03-24T00:00:00Z', last_run_at: null }],
      audit: [],
      insights: null,
    })

    expect(model.nodes.map(node => node.id)).toContain('eve')
    expect(model.nodes.some(node => node.kind === 'thread')).toBe(true)
    expect(model.nodes.some(node => node.kind === 'trigger')).toBe(true)
  })
})

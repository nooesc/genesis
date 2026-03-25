import { describe, expect, it } from 'vitest'
import type { CommandMapModel } from './types'
import {
  buildCommandMapSearchIndex,
  buildCommandMapJumpTarget,
  filterCommandMapSearchIndex,
} from './search'

const model: CommandMapModel = {
  nodes: [
    {
      id: 'eve',
      kind: 'eve',
      layer: 'core',
      ring: 0,
      label: 'Eve',
      subtitle: 'ok · 1.0.0',
      status: 'ok',
      position: { x: 0, y: 0 },
    },
    {
      id: 'session-s1',
      kind: 'thread',
      layer: 'execution',
      ring: 1,
      label: 'Alpha',
      subtitle: 'api · 15 tok',
      status: 'ok',
      position: { x: 10, y: 20 },
    },
    {
      id: 'schedule-nightly',
      kind: 'trigger',
      layer: 'trigger',
      ring: 2,
      label: 'nightly',
      subtitle: '0 0 * * * · api',
      status: 'idle',
      position: { x: 20, y: 30 },
    },
    {
      id: 'skill-deploy-service',
      kind: 'recipe',
      layer: 'recipe',
      ring: 2,
      label: 'deploy-service',
      subtitle: 'Run the standard deployment recipe',
      status: 'ok',
      position: { x: 25, y: 35 },
    },
    {
      id: 'alert-a1',
      kind: 'alert',
      layer: 'alert',
      ring: 4,
      label: 'stuck_loop',
      subtitle: 'session s1 · shell · failed',
      status: 'error',
      position: { x: 40, y: 50 },
    },
  ],
  edges: [],
}

describe('command map search', () => {
  it('indexes nodes by label, type, layer, and status', () => {
    const index = buildCommandMapSearchIndex(model)
    const alpha = index.find(entry => entry.nodeId === 'session-s1')

    expect(alpha).toBeDefined()
    expect(alpha?.keywords).toContain('thread')
    expect(alpha?.keywords).toContain('execution')
    expect(alpha?.keywords).toContain('ok')
  })

  it('can filter results by query, layer, and kind', () => {
    const index = buildCommandMapSearchIndex(model)
    const filtered = filterCommandMapSearchIndex(index, {
      query: 'api',
      layer: 'execution',
      kind: 'thread',
    })

    expect(filtered.map(entry => entry.nodeId)).toEqual(['session-s1'])
  })

  it('can filter recipe results distinctly from triggers', () => {
    const index = buildCommandMapSearchIndex(model)
    const filtered = filterCommandMapSearchIndex(index, {
      query: 'deployment',
      layer: 'recipe',
      kind: 'recipe',
    })

    expect(filtered.map(entry => entry.nodeId)).toEqual(['skill-deploy-service'])
  })

  it('builds a monitor jump target for a selected node', () => {
    expect(buildCommandMapJumpTarget('schedule-nightly')).toEqual({
      to: '/monitor',
      search: {
        focusNodeId: 'schedule-nightly',
        focusMode: 'focus',
      },
    })
  })
})

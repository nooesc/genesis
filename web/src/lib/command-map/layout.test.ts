import { describe, expect, it } from 'vitest'
import { applyCommandMapLayout, COMMAND_MAP_RINGS, orderForLayout } from './layout'
import type { CommandMapNode } from './types'

const baseNodes: CommandMapNode[] = [
  {
    id: 'eve',
    kind: 'eve',
    layer: 'core',
    ring: 0,
    label: 'Eve',
    status: 'ok',
    position: { x: 0, y: 0 },
  },
  {
    id: 'session-a',
    kind: 'thread',
    layer: 'execution',
    ring: 1,
    label: 'Alpha',
    status: 'ok',
    position: { x: 0, y: 0 },
  },
  {
    id: 'session-b',
    kind: 'thread',
    layer: 'execution',
    ring: 1,
    label: 'Beta',
    status: 'ok',
    position: { x: 0, y: 0 },
  },
  {
    id: 'system-model',
    kind: 'system',
    layer: 'system',
    ring: 3,
    label: 'Model',
    status: 'ok',
    position: { x: 0, y: 0 },
  },
]

describe('applyCommandMapLayout', () => {
  it('orders trigger and recipe nodes by explicit layer within the shared middle ring', () => {
    const ordered = orderForLayout([
      {
        id: 'skill-deploy-service',
        kind: 'recipe',
        layer: 'recipe',
        ring: COMMAND_MAP_RINGS.recipe,
        label: 'deploy-service',
        status: 'ok',
        position: { x: 0, y: 0 },
      },
      {
        id: 'schedule-nightly',
        kind: 'trigger',
        layer: 'trigger',
        ring: COMMAND_MAP_RINGS.trigger,
        label: 'nightly',
        status: 'ok',
        position: { x: 0, y: 0 },
      },
    ])

    expect(ordered.map(node => node.kind)).toEqual(['trigger', 'recipe'])
  })

  it('uses pinned positions instead of auto-layout positions', () => {
    const pinned = {
      'system-model': { x: 640, y: -120 },
    }

    const model = applyCommandMapLayout(baseNodes, pinned)
    const pinnedNode = model.find(node => node.id === 'system-model')

    expect(pinnedNode?.pinned).toBe(true)
    expect(pinnedNode?.position).toEqual({ x: 640, y: -120 })
  })

  it('keeps pinned nodes stable while unpinned nodes reflow', () => {
    const pinned = {
      'session-a': { x: 250, y: 10 },
    }

    const initial = applyCommandMapLayout(baseNodes, pinned)
    const withExtraNode = applyCommandMapLayout(
      [
        ...baseNodes,
        {
          id: 'session-c',
          kind: 'thread',
          layer: 'execution',
          ring: 1,
          label: 'Gamma',
          status: 'ok',
          position: { x: 0, y: 0 },
        },
      ],
      pinned,
    )

    const initialPinned = initial.find(node => node.id === 'session-a')
    const nextPinned = withExtraNode.find(node => node.id === 'session-a')
    const initialUnpinned = initial.find(node => node.id === 'session-b')
    const nextUnpinned = withExtraNode.find(node => node.id === 'session-b')

    expect(nextPinned?.position).toEqual(initialPinned?.position)
    expect(nextPinned?.pinned).toBe(true)
    expect(nextUnpinned?.position).not.toEqual(initialUnpinned?.position)
  })
})

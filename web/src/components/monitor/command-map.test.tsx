import type { ComponentType, ReactNode } from 'react'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { CommandMap } from './command-map'
import type { CommandMapModel } from '@/lib/command-map/types'
import {
  loadCommandMapPinnedPositions,
  saveCommandMapPinnedPositions,
} from '@/lib/command-map/storage'
import { installStorageMock } from '@/test/storage-mock'

vi.mock('@xyflow/react', async () => {
  const ReactModule = await import('react')

  return {
    Background: () => null,
    BackgroundVariant: { Dots: 'dots' },
    Controls: () => null,
    Handle: () => null,
    MarkerType: { ArrowClosed: 'arrowclosed' },
    Position: { Top: 'top', Bottom: 'bottom' },
    ReactFlow: ({
      nodes,
      nodeTypes,
      onNodeDragStop,
      children,
    }: {
      nodes: Array<{ id: string; type: string; data: unknown; position: { x: number; y: number } }>
      nodeTypes: Record<string, ComponentType<{ id: string; data: unknown }>>
      onNodeDragStop?: (event: unknown, node: { id: string; position: { x: number; y: number } }) => void
      children?: ReactNode
    }) => (
      <div data-testid="react-flow">
        {nodes.map(node => {
          const NodeComponent = nodeTypes[node.type]
          return (
            <div key={node.id} data-testid={`node-${node.id}`}>
              <NodeComponent id={node.id} data={node.data} />
              {onNodeDragStop && (
                <button
                  type="button"
                  aria-label={`Drag ${node.id}`}
                  onClick={() => onNodeDragStop(null, {
                    id: node.id,
                    position: {
                      x: node.position.x + 48,
                      y: node.position.y + 24,
                    },
                  })}
                >
                  Drag {node.id}
                </button>
              )}
            </div>
          )
        })}
        {children}
      </div>
    ),
    useEdgesState: (initialEdges: unknown[]) => {
      const [edges, setEdges] = ReactModule.useState(initialEdges)
      return [edges, setEdges, vi.fn()] as const
    },
    useNodesState: (initialNodes: unknown[]) => {
      const [nodes, setNodes] = ReactModule.useState(initialNodes)
      return [nodes, setNodes, vi.fn()] as const
    },
  }
})

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
      data: { model: 'gpt-4.1' },
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
      data: { session_id: 's1', platform: 'api', total_input_tokens: 10, total_output_tokens: 5, index: 0 },
    },
    {
      id: 'schedule-nightly',
      kind: 'trigger',
      layer: 'trigger',
      ring: 2,
      label: 'nightly',
      subtitle: '0 0 * * * · api',
      status: 'ok',
      position: { x: 20, y: 30 },
      data: { schedule_id: 'nightly', enabled: true, prompt: 'Run nightly', last_run_at: null },
    },
    {
      id: 'skill-deploy-service',
      kind: 'recipe',
      layer: 'recipe',
      ring: 2,
      label: 'deploy-service',
      subtitle: 'Run the standard deployment recipe',
      status: 'ok',
      position: { x: 24, y: 34 },
      data: { skill_name: 'deploy-service', tag_count: 2 },
    },
    {
      id: 'system-model',
      kind: 'system',
      layer: 'system',
      ring: 3,
      label: 'Model',
      subtitle: 'gpt-4.1',
      status: 'ok',
      position: { x: 30, y: 40 },
      data: { model: 'gpt-4.1' },
    },
  ],
  edges: [],
}

const offlineModel: CommandMapModel = {
  nodes: [
    {
      ...model.nodes[0],
      subtitle: 'offline · gateway unavailable',
      status: 'error',
      data: { model: null },
    },
  ],
  edges: [],
}

describe('CommandMap', () => {
  beforeEach(() => {
    installStorageMock()
  })

  it('renders Eve, layer toggles, and inspector state', async () => {
    render(<CommandMap model={model} />)

    expect(await screen.findByRole('button', { name: /^Eve(?:\s|$)/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Execution/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Recipes/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Triggers/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Declutter/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Focus/i })).toBeInTheDocument()
    expect(screen.getByText(/select a node to inspect/i)).toBeInTheDocument()

    expect(await screen.findByRole('button', { name: /^deploy-service(?:\s|$)/i })).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /Recipes/i }))
    expect(screen.queryByRole('button', { name: /^deploy-service(?:\s|$)/i })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: /^nightly(?:\s|$)/i })).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: /Recipes/i }))
    expect(await screen.findByRole('button', { name: /^deploy-service(?:\s|$)/i })).toBeInTheDocument()

    fireEvent.click(await screen.findByRole('button', { name: /^Alpha(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Focus/i }))

    expect(screen.getByRole('heading', { name: /Alpha/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Focus/i })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getAllByText(/session-s1/i)).not.toHaveLength(0)
  })

  it('clears selection and focus when the selected layer is hidden', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^Alpha(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Focus/i }))
    fireEvent.click(screen.getByRole('button', { name: /Execution/i }))

    expect(screen.getByText(/select a node to inspect/i)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Focus/i })).toHaveAttribute('aria-pressed', 'false')
    expect(screen.queryByRole('button', { name: /^Alpha(?:\s|$)/i })).not.toBeInTheDocument()
  })

  it('does not render a dead search control', () => {
    render(<CommandMap model={model} />)

    expect(screen.queryByRole('button', { name: /Search/i })).not.toBeInTheDocument()
  })

  it('renders offline Eve with an error tone', async () => {
    render(<CommandMap model={offlineModel} />)

    const eve = await screen.findByRole('button', { name: /^Eve(?:\s|$)/i })
    expect(eve).toHaveTextContent(/offline · gateway unavailable/i)
    expect(eve.className).toContain('border-red-400/40')
  })

  it('pins dragged nodes into persisted layout storage', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(screen.getByRole('button', { name: /^Drag session-s1$/i }))

    await waitFor(() => {
      expect(loadCommandMapPinnedPositions()).toMatchObject({
        'session-s1': { x: expect.any(Number), y: expect.any(Number) },
      })
    })
  })

  it('prunes stale pinned positions when the topology no longer contains them', async () => {
    saveCommandMapPinnedPositions({
      'session-a': { x: 320, y: -24 },
      'alert-old': { x: -80, y: 10 },
    })

    render(<CommandMap model={offlineModel} />)

    await waitFor(() => {
      expect(loadCommandMapPinnedPositions()).toEqual({})
    })
  })
})

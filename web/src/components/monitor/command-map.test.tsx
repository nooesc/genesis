import type { ComponentType, ReactNode } from 'react'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { CommandMap } from './command-map'
import type { CommandMapModel } from '@/lib/command-map/types'
import {
  loadCommandMapPinnedPositions,
  saveCommandMapPinnedPositions,
} from '@/lib/command-map/storage'
import { useIsMobile } from '@/hooks/use-mobile'
import { installStorageMock } from '@/test/storage-mock'

vi.mock('@/hooks/use-mobile', () => ({
  useIsMobile: vi.fn(),
}))

vi.mock('@tanstack/react-router', async () => {
  const actual = await vi.importActual<typeof import('@tanstack/react-router')>('@tanstack/react-router')
  return {
    ...actual,
    useNavigate: () => vi.fn(),
  }
})

const fitViewMock = vi.fn()
let currentFlowNodes: Array<{ id: string; position: { x: number; y: number } }> = []

vi.mock('@/lib/api/queries/skills', () => ({
  useSkill: () => ({
    data: {
      name: 'deploy-service',
      description: 'Run the standard deployment recipe',
      instructions: 'Deploy the service with the approved steps.',
      tags: ['ops', 'release'],
      trigger_hint: 'manual',
      version: '2',
      created_at: '2026-03-24T00:00:00Z',
      updated_at: '2026-03-24T00:00:00Z',
    },
    isLoading: false,
  }),
  useSkillUsage: () => ({
    data: {
      stats: { total_uses: 3, last_used_at: '2026-03-24T00:10:00Z', avg_duration_ms: 120 },
      recent: [],
    },
    isLoading: false,
  }),
}))

vi.mock('@/lib/api/queries/schedules', () => ({
  useSchedules: () => ({
    data: [
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
    isLoading: false,
  }),
}))

const toggleScheduleMock = vi.fn()

vi.mock('@/lib/api/mutations/schedules', () => ({
  useToggleSchedule: () => ({
    mutate: toggleScheduleMock,
    isPending: false,
  }),
}))

vi.mock('@/lib/api/queries/sessions', () => ({
  useSession: () => ({
    data: {
      id: 's1',
      title: 'Alpha',
      platform: 'api',
      total_input_tokens: 10,
      total_output_tokens: 5,
      parent_session_id: null,
      created_at: '2026-03-24T00:00:00Z',
      updated_at: '2026-03-24T00:00:00Z',
    },
    isLoading: false,
  }),
  useMessages: () => ({
    data: [
      {
        id: 'm1',
        session_id: 's1',
        role: 'user',
        content: 'Hello there',
        tool_call_id: null,
        tool_calls_json: null,
        created_at: '2026-03-24T00:01:00Z',
      },
    ],
    isLoading: false,
  }),
}))

vi.mock('@/lib/api/queries/audit', () => ({
  useAuditLog: () => ({
    data: [
      {
        id: 'a1',
        event_type: 'stuck_loop',
        session_id: 's1',
        details: '{"tool":"shell","failure_count":3}',
        created_at: '2026-03-24T00:00:00Z',
      },
    ],
    isLoading: false,
  }),
}))

vi.mock('@xyflow/react', async () => {
  const ReactModule = await import('react')

  return {
    Background: () => null,
    BackgroundVariant: { Dots: 'dots' },
    Controls: () => null,
    Handle: () => null,
    MarkerType: { ArrowClosed: 'arrowclosed' },
    Position: { Top: 'top', Bottom: 'bottom' },
    useReactFlow: () => ({
      viewportInitialized: true,
      getNode: (id: string) => currentFlowNodes.find(node => node.id === id),
      fitView: fitViewMock,
    }),
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
    }) => {
      currentFlowNodes = nodes

      return (
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
      )
    },
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
    vi.mocked(useIsMobile).mockReturnValue(false)
    fitViewMock.mockReset()
    currentFlowNodes = []
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

  it('centers the viewport when a focus node jump lands', async () => {
    render(<CommandMap model={model} focusNodeId="session-s1" focusMode="focus" />)

    await waitFor(() => {
      expect(fitViewMock).toHaveBeenCalled()
    })

    expect(fitViewMock).toHaveBeenCalledWith(expect.objectContaining({
      nodes: [{ id: 'session-s1' }],
      duration: 250,
      padding: 0.35,
    }))
  })

  it('does not recenter the viewport for ordinary local selection', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^Alpha(?:\s|$)/i }))

    expect(fitViewMock).not.toHaveBeenCalled()
  })

  it('opens a recipe details dialog from the inspector', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^deploy-service(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Recipe details/i }))

    expect(await screen.findByRole('dialog')).toHaveTextContent(/deploy-service/i)
  })

  it('opens a trigger dialog from the inspector', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^nightly(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Trigger details/i }))

    expect(await screen.findByRole('dialog')).toHaveTextContent(/nightly/i)
  })

  it('opens a thread details dialog from the inspector', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^Alpha(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Thread details/i }))

    expect(await screen.findByRole('dialog')).toHaveTextContent(/Alpha/i)
  })

  it('opens an event log drawer from the inspector', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^Model(?:\s|$)/i }))
    fireEvent.click(screen.getByRole('button', { name: /Event log/i }))

    expect(await screen.findByRole('dialog')).toHaveTextContent(/Event log/i)
  })

  it('uses a sheet on mobile when a node is selected', async () => {
    vi.mocked(useIsMobile).mockReturnValue(true)

    render(<CommandMap model={model} />)

    fireEvent.click(await screen.findByRole('button', { name: /^Alpha(?:\s|$)/i }))

    expect(await screen.findByRole('dialog')).toHaveTextContent(/Alpha/i)
    expect(screen.queryByText(/Select a node to inspect/i)).not.toBeInTheDocument()
  })

  it('keeps recipes visible during declutter alongside triggers', async () => {
    render(<CommandMap model={model} />)

    fireEvent.click(screen.getByRole('button', { name: /Declutter/i }))

    expect(await screen.findByRole('button', { name: /^nightly(?:\s|$)/i })).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: /^deploy-service(?:\s|$)/i })).toBeInTheDocument()
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

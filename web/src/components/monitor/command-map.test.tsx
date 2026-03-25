import { fireEvent, render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { CommandMap } from './command-map'
import type { CommandMapModel } from '@/lib/command-map/types'

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

describe('CommandMap', () => {
  it('renders Eve, layer toggles, and inspector state', () => {
    render(<CommandMap model={model} />)

    expect(screen.getByRole('button', { name: /Eve/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Execution/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Declutter/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Focus/i })).toBeInTheDocument()
    expect(screen.getByText(/select a node to inspect/i)).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /Declutter/i }))
    expect(screen.queryByRole('button', { name: /Model/i })).not.toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /Declutter/i }))
    expect(screen.getByRole('button', { name: /Model/i })).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: /Alpha/i }))
    fireEvent.click(screen.getByRole('button', { name: /Focus/i }))

    expect(screen.getByRole('heading', { name: /Alpha/i })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Focus/i })).toHaveAttribute('aria-pressed', 'true')
    expect(screen.getByText(/session-s1/i)).toBeInTheDocument()
  })
})

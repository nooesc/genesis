import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { CommandMapInspector } from './command-map-inspector'

describe('CommandMapInspector', () => {
  it('uses payload-specific facts for system nodes', () => {
    render(
      <CommandMapInspector
        selectedNode={{
          id: 'system-mcp',
          kind: 'system',
          layer: 'system',
          ring: 3,
          label: 'MCP',
          subtitle: '2 connected servers',
          status: 'ok',
          data: { mcp_servers: 2 },
          position: { x: 0, y: 0 },
        }}
        onOpenRecipeDetails={vi.fn()}
        onOpenTriggerDetails={vi.fn()}
        onOpenThreadDetails={vi.fn()}
        onOpenEventLog={vi.fn()}
      />,
    )

    expect(screen.getByText(/MCP servers/i)).toBeInTheDocument()
    expect(screen.getByText(/^2$/i)).toBeInTheDocument()
    expect(screen.queryByText(/^Model$/i)).not.toBeInTheDocument()
  })
})

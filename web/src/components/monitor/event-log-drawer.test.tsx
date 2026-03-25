import { render, screen } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { EventLogDrawer } from './event-log-drawer'

const useAuditLogMock = vi.fn()

vi.mock('@/lib/api/queries/audit', () => ({
  useAuditLog: (...args: unknown[]) => useAuditLogMock(...args),
}))

describe('EventLogDrawer', () => {
  beforeEach(() => {
    useAuditLogMock.mockReset()
  })

  it('shows a loading state while audit entries are still loading', () => {
    useAuditLogMock.mockReturnValue({
      data: [],
      isLoading: true,
    })

    render(<EventLogDrawer open onOpenChange={() => {}} title="Eve logs" />)

    expect(screen.getByText(/Loading event log/i)).toBeInTheDocument()
    expect(screen.queryByText(/No matching events found/i)).not.toBeInTheDocument()
  })
})

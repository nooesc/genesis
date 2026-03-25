import { describe, expect, it } from 'vitest'
import { shouldShowMonitorSkeleton } from '@/routes/monitor.lazy'

describe('shouldShowMonitorSkeleton', () => {
  it('does not block the monitor skeleton on skills loading', () => {
    expect(
      shouldShowMonitorSkeleton({
        healthLoading: false,
        insightsLoading: false,
        sessionsLoading: false,
        schedulesLoading: false,
        auditLoading: false,
        skillsLoading: true,
      }),
    ).toBe(false)
  })
})

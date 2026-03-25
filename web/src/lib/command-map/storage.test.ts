import { beforeEach, describe, expect, it } from 'vitest'
import {
  clearCommandMapPinnedPositions,
  loadCommandMapPinnedPositions,
  saveCommandMapPinnedPositions,
} from './storage'
import { installStorageMock } from '@/test/storage-mock'

describe('command map storage', () => {
  beforeEach(() => {
    installStorageMock()
    localStorage.clear()
  })

  it('persists and reloads pinned positions', () => {
    saveCommandMapPinnedPositions({
      eve: { x: 0, y: 0 },
      'session-a': { x: 280, y: -32 },
    })

    expect(loadCommandMapPinnedPositions()).toEqual({
      eve: { x: 0, y: 0 },
      'session-a': { x: 280, y: -32 },
    })
  })

  it('clears stored positions during layout reset', () => {
    saveCommandMapPinnedPositions({
      'session-a': { x: 280, y: -32 },
    })

    clearCommandMapPinnedPositions()

    expect(loadCommandMapPinnedPositions()).toEqual({})
  })
})

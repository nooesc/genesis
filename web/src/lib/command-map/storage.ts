import type { CommandMapPoint } from './types'

const COMMAND_MAP_PINNED_POSITIONS_KEY = 'genesis.command-map.pinned-positions.v1'

export type CommandMapPinnedPositions = Record<string, CommandMapPoint>

function isCommandMapPoint(value: unknown): value is CommandMapPoint {
  if (!value || typeof value !== 'object') return false

  const candidate = value as Partial<CommandMapPoint>
  return typeof candidate.x === 'number' && Number.isFinite(candidate.x)
    && typeof candidate.y === 'number' && Number.isFinite(candidate.y)
}

export function loadCommandMapPinnedPositions(): CommandMapPinnedPositions {
  if (typeof window === 'undefined') return {}

  try {
    const raw = window.localStorage.getItem(COMMAND_MAP_PINNED_POSITIONS_KEY)
    if (!raw) return {}

    const parsed = JSON.parse(raw)
    if (!parsed || typeof parsed !== 'object') return {}

    return Object.fromEntries(
      Object.entries(parsed).filter(([, value]) => isCommandMapPoint(value)),
    ) as CommandMapPinnedPositions
  } catch {
    return {}
  }
}

export function saveCommandMapPinnedPositions(positions: CommandMapPinnedPositions): void {
  if (typeof window === 'undefined') return
  window.localStorage.setItem(COMMAND_MAP_PINNED_POSITIONS_KEY, JSON.stringify(positions))
}

export function clearCommandMapPinnedPositions(): void {
  if (typeof window === 'undefined') return
  window.localStorage.removeItem(COMMAND_MAP_PINNED_POSITIONS_KEY)
}

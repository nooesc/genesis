import { useEffect } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { navRoutes } from './nav'

/** Map shortcut keys to routes */
const shortcutMap = new Map(
  navRoutes.filter(r => r.shortcut).map(r => [r.shortcut!, r.to])
)

interface UseKeyboardNavOptions {
  onToggleHelp: () => void
  onToggleCommandPalette: () => void
}

/**
 * Global keyboard navigation:
 * - 1-9, 0: navigate to dock items
 * - ?: toggle shortcut help
 * - /: focus search input on current page
 */
export function useKeyboardNav({ onToggleHelp, onToggleCommandPalette }: UseKeyboardNavOptions) {
  const navigate = useNavigate()

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      // Skip if user is typing in an input/textarea/contenteditable
      const target = e.target as HTMLElement
      if (
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.tagName === 'SELECT' ||
        target.isContentEditable
      ) {
        return
      }

      // Skip if any modifier is held (except for Cmd+K which is handled elsewhere)
      if (e.metaKey || e.ctrlKey || e.altKey) return

      // Number keys: dock navigation
      const route = shortcutMap.get(e.key)
      if (route) {
        e.preventDefault()
        void navigate({ to: route })
        return
      }

      // ? = toggle help overlay
      if (e.key === '?') {
        e.preventDefault()
        onToggleHelp()
        return
      }

      // / = focus search input
      if (e.key === '/') {
        e.preventDefault()
        const searchInput = document.querySelector<HTMLInputElement>('input[placeholder*="earch"]')
        searchInput?.focus()
        return
      }

      // k = open command palette (without modifier, as alternative)
      if (e.key === 'k') {
        e.preventDefault()
        onToggleCommandPalette()
        return
      }
    }

    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [navigate, onToggleHelp, onToggleCommandPalette])
}

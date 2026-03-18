import { createRootRoute, Outlet } from '@tanstack/react-router'
import { SystemBar } from '@/components/layout/system-bar'
import { Dock } from '@/components/layout/dock'
import { CommandPalette } from '@/components/layout/command-palette'
import { ShortcutHelp } from '@/components/layout/shortcut-help'
import { ConnectionBanner } from '@/components/shared/connection-banner'
import { useKeyboardNav } from '@/lib/use-keyboard-nav'
import { useCallback, useState } from 'react'

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  const [commandOpen, setCommandOpen] = useState(false)
  const [helpOpen, setHelpOpen] = useState(false)

  const toggleHelp = useCallback(() => setHelpOpen(v => !v), [])
  const toggleCommand = useCallback(() => setCommandOpen(v => !v), [])

  const closeHelp = useCallback(() => setHelpOpen(false), [])

  useKeyboardNav({
    onToggleHelp: toggleHelp,
    onCloseHelp: closeHelp,
    onToggleCommandPalette: toggleCommand,
    helpOpen,
  })

  return (
    <div className="flex h-screen flex-col bg-background">
      <SystemBar />
      <ConnectionBanner />
      <main className="flex-1 overflow-auto p-6">
        <Outlet />
      </main>
      <Dock onCommandPalette={() => setCommandOpen(true)} />
      <CommandPalette open={commandOpen} onOpenChange={setCommandOpen} />
      <ShortcutHelp open={helpOpen} onClose={() => setHelpOpen(false)} />
    </div>
  )
}

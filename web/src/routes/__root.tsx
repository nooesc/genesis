import { createRootRoute, Outlet, useRouterState } from '@tanstack/react-router'
import { SystemBar } from '@/components/layout/system-bar'
import { Dock } from '@/components/layout/dock'
import { CommandPalette } from '@/components/layout/command-palette'
import { ShortcutHelp } from '@/components/layout/shortcut-help'
import { ConnectionBanner } from '@/components/shared/connection-banner'
import { Toaster } from '@/components/ui/sonner'
import { useKeyboardNav } from '@/lib/use-keyboard-nav'
import { useCallback, useState } from 'react'

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  const [commandOpen, setCommandOpen] = useState(false)
  const [helpOpen, setHelpOpen] = useState(false)

  // Only subscribe to pathname changes to avoid unnecessary re-renders
  const pageKey = useRouterState({ select: (s) => s.location.pathname })

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
        <div key={pageKey} className="page-enter h-full">
          <Outlet />
        </div>
      </main>
      <Dock onCommandPalette={() => setCommandOpen(true)} />
      <CommandPalette open={commandOpen} onOpenChange={setCommandOpen} />
      <ShortcutHelp open={helpOpen} onClose={() => setHelpOpen(false)} />
      <Toaster />
    </div>
  )
}

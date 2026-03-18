import { createRootRoute, Outlet } from '@tanstack/react-router'
import { SystemBar } from '@/components/layout/system-bar'
import { Dock } from '@/components/layout/dock'
import { CommandPalette } from '@/components/layout/command-palette'
import { ConnectionBanner } from '@/components/shared/connection-banner'
import { useState } from 'react'

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  const [commandOpen, setCommandOpen] = useState(false)

  return (
    <div className="flex h-screen flex-col bg-background">
      <SystemBar />
      <ConnectionBanner />
      <main className="flex-1 overflow-auto p-6">
        <Outlet />
      </main>
      <Dock onCommandPalette={() => setCommandOpen(true)} />
      <CommandPalette open={commandOpen} onOpenChange={setCommandOpen} />
    </div>
  )
}

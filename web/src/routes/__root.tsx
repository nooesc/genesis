import { createRootRoute, Outlet } from '@tanstack/react-router'
import { Sidebar } from '@/components/layout/sidebar'
import { Topbar } from '@/components/layout/topbar'
import { ConnectionBanner } from '@/components/shared/connection-banner'

export const Route = createRootRoute({
  component: RootLayout,
})

function RootLayout() {
  return (
    <div className="flex h-screen bg-background">
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <Topbar />
        <ConnectionBanner />
        <main className="flex-1 overflow-auto p-6">
          <Outlet />
        </main>
      </div>
    </div>
  )
}

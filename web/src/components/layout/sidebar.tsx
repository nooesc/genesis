import { Link, useRouterState } from '@tanstack/react-router'
import {
  LayoutDashboard, MessagesSquare, Brain, Database,
  Clock, Wrench, BarChart3, FileText, Settings,
} from 'lucide-react'

const navItems = [
  { to: '/', label: 'Dashboard', icon: LayoutDashboard },
  { to: '/sessions', label: 'Sessions', icon: MessagesSquare },
  { to: '/skills', label: 'Skills', icon: Brain },
  { to: '/memories', label: 'Memories', icon: Database },
  { to: '/schedules', label: 'Schedules', icon: Clock },
  { to: '/tools', label: 'Tools', icon: Wrench },
  { to: '/analytics', label: 'Analytics', icon: BarChart3 },
  { to: '/audit', label: 'Audit Log', icon: FileText },
  { to: '/settings', label: 'Settings', icon: Settings },
] as const

export function Sidebar() {
  const router = useRouterState()
  const currentPath = router.location.pathname

  return (
    <aside className="flex w-56 flex-col border-r border-border bg-card">
      <div className="flex h-14 items-center border-b border-border px-4">
        <span className="font-mono text-sm font-bold text-primary">GENESIS</span>
      </div>
      <nav className="flex-1 space-y-1 p-2">
        {navItems.map(({ to, label, icon: Icon }) => {
          const isActive = to === '/' ? currentPath === '/' : currentPath.startsWith(to)
          return (
            <Link
              key={to}
              to={to}
              className={`flex items-center gap-3 rounded-md px-3 py-2 text-sm transition-colors ${isActive ? 'bg-accent text-foreground' : 'text-muted-foreground hover:bg-accent hover:text-foreground'}`}
            >
              <Icon className="h-4 w-4" />
              {label}
            </Link>
          )
        })}
      </nav>
    </aside>
  )
}

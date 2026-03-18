import { Link, useRouterState } from '@tanstack/react-router'
import {
  LayoutDashboard, MessagesSquare, Brain, Database,
  Clock, Wrench, BarChart3, FileText, Settings, Command,
} from 'lucide-react'
import { useState } from 'react'

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

interface DockProps {
  onCommandPalette: () => void
}

export function Dock({ onCommandPalette }: DockProps) {
  const router = useRouterState()
  const currentPath = router.location.pathname
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  return (
    <nav className="dock relative flex h-[52px] items-center justify-center border-t border-border/40 bg-[#0c0c0c]/95 backdrop-blur-sm">
      <div className="flex items-center gap-1">
        {navItems.map(({ to, label, icon: Icon }, index) => {
          const isActive = to === '/' ? currentPath === '/' : currentPath.startsWith(to)
          const isHovered = hoveredIndex === index

          return (
            <div key={to} className="relative">
              {/* Tooltip */}
              {isHovered && (
                <div className="absolute -top-8 left-1/2 -translate-x-1/2 whitespace-nowrap rounded bg-card px-2 py-0.5 font-mono text-[10px] text-foreground/80 shadow-lg ring-1 ring-border/50 pointer-events-none">
                  {label}
                </div>
              )}

              <Link
                to={to}
                className={`dock-item relative flex h-9 w-9 items-center justify-center rounded-lg transition-all duration-200 ${
                  isActive
                    ? 'text-primary'
                    : 'text-muted-foreground/60 hover:text-foreground/80'
                }`}
                style={{
                  transform: isHovered ? 'scale(1.2) translateY(-2px)' : 'scale(1)',
                }}
                onMouseEnter={() => setHoveredIndex(index)}
                onMouseLeave={() => setHoveredIndex(null)}
              >
                <Icon className="h-[18px] w-[18px]" strokeWidth={isActive ? 2 : 1.5} />

                {/* Active indicator dot */}
                {isActive && (
                  <div className="absolute -bottom-1 left-1/2 h-[3px] w-[3px] -translate-x-1/2 rounded-full bg-primary shadow-[0_0_4px_rgba(8,145,178,0.6)]" />
                )}
              </Link>
            </div>
          )
        })}

        {/* Separator */}
        <div className="mx-1.5 h-5 w-px bg-border/30" />

        {/* Command palette trigger */}
        <button
          onClick={onCommandPalette}
          className="dock-item flex h-9 items-center gap-1 rounded-lg px-2 text-muted-foreground/40 transition-all duration-200 hover:text-foreground/60"
          title="Command Palette (⌘K)"
        >
          <Command className="h-3.5 w-3.5" />
          <span className="font-mono text-[9px] tracking-wider">⌘K</span>
        </button>
      </div>
    </nav>
  )
}

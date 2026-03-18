import { Link, useRouterState } from '@tanstack/react-router'
import { Command } from 'lucide-react'
import { useState } from 'react'
import { navRoutes } from '@/lib/nav'

interface DockProps {
  onCommandPalette: () => void
}

export function Dock({ onCommandPalette }: DockProps) {
  const router = useRouterState()
  const currentPath = router.location.pathname
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  return (
    <nav
      className="dock relative flex h-[52px] items-center justify-center border-t border-border/40 bg-[#0c0c0c]/95 backdrop-blur-sm"
      aria-label="Main navigation"
    >
      <div className="flex items-center gap-1 overflow-x-auto px-2 no-scrollbar">
        {navRoutes.map(({ to, label, icon: Icon }, index) => {
          const isActive = to === '/' ? currentPath === '/' : currentPath.startsWith(to)
          const isHovered = hoveredIndex === index

          return (
            <div key={to} className="relative shrink-0">
              {/* Tooltip */}
              {isHovered && (
                <div className="absolute -top-8 left-1/2 -translate-x-1/2 whitespace-nowrap rounded bg-card px-2 py-0.5 font-mono text-[10px] text-foreground/80 shadow-lg ring-1 ring-border/50 pointer-events-none" role="tooltip">
                  {label}
                  {navRoutes[index].shortcut && (
                    <span className="ml-1.5 text-muted-foreground/40">{navRoutes[index].shortcut}</span>
                  )}
                </div>
              )}

              <Link
                to={to}
                aria-label={label}
                aria-current={isActive ? 'page' : undefined}
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
        <div className="mx-1.5 h-5 w-px shrink-0 bg-border/30" />

        {/* Command palette trigger */}
        <button
          onClick={onCommandPalette}
          className="dock-item flex h-9 shrink-0 items-center gap-1 rounded-lg px-2 text-muted-foreground/40 transition-all duration-200 hover:text-foreground/60"
          aria-label="Open command palette"
          title="Command Palette (⌘K)"
        >
          <Command className="h-3.5 w-3.5" />
          <span className="hidden font-mono text-[9px] tracking-wider sm:inline">⌘K</span>
        </button>
      </div>
    </nav>
  )
}

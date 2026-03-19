import { Link, useRouterState } from '@tanstack/react-router'
import { Command } from 'lucide-react'
import { useState } from 'react'
import { navRoutes } from '@/lib/nav'

interface DockProps {
  onCommandPalette: () => void
}

/** macOS-style proximity scale: hovered item scales 1.25, neighbors scale 1.1 */
function getDockScale(hoveredIndex: number | null, itemIndex: number): string {
  if (hoveredIndex === null) return 'scale(1)'
  const dist = Math.abs(hoveredIndex - itemIndex)
  if (dist === 0) return 'scale(1.25) translateY(-3px)'
  if (dist === 1) return 'scale(1.1) translateY(-1px)'
  return 'scale(1)'
}

export function Dock({ onCommandPalette }: DockProps) {
  const router = useRouterState()
  const currentPath = router.location.pathname
  const [hoveredIndex, setHoveredIndex] = useState<number | null>(null)

  return (
    <nav
      className="dock relative flex h-[52px] items-center justify-center border-t border-border/20 bg-[#0a0a0a]/90 backdrop-blur-md"
      aria-label="Main navigation"
    >
      {/* Top glow line — mirrors system bar bottom glow */}
      <div className="pointer-events-none absolute inset-x-0 top-0 h-px bg-gradient-to-r from-transparent via-primary/15 to-transparent" />

      <div className="flex items-center gap-1 overflow-x-auto px-2 no-scrollbar">
        {navRoutes.map(({ to, label, icon: Icon }, index) => {
          const isActive = to === '/' ? currentPath === '/' : currentPath.startsWith(to)

          return (
            <div key={to} className="relative shrink-0">
              {/* Tooltip — animated fade-in */}
              {hoveredIndex === index && (
                <div className="dock-tooltip absolute -top-9 left-1/2 whitespace-nowrap rounded-md bg-[#0d0d0d] px-2.5 py-1 font-mono text-[10px] text-foreground/80 shadow-xl ring-1 ring-border/30 pointer-events-none" role="tooltip">
                  {label}
                  {navRoutes[index].shortcut && (
                    <kbd className="ml-2 inline-flex h-4 min-w-[14px] items-center justify-center rounded border border-border/30 bg-muted/20 px-1 font-mono text-[8px] text-muted-foreground/50">
                      {navRoutes[index].shortcut}
                    </kbd>
                  )}
                </div>
              )}

              <Link
                to={to}
                aria-label={label}
                aria-current={isActive ? 'page' : undefined}
                className={`dock-item relative flex h-9 w-9 items-center justify-center rounded-lg ${
                  isActive
                    ? 'text-primary'
                    : 'text-muted-foreground/50 hover:text-foreground/80'
                }`}
                style={{ transform: getDockScale(hoveredIndex, index) }}
                onMouseEnter={() => setHoveredIndex(index)}
                onMouseLeave={() => setHoveredIndex(null)}
              >
                <Icon className="h-[18px] w-[18px]" strokeWidth={isActive ? 2 : 1.5} />

                {/* Active indicator dot with glow */}
                {isActive && (
                  <div className="absolute -bottom-1 left-1/2 h-[3px] w-[3px] -translate-x-1/2 rounded-full bg-primary shadow-[0_0_6px_rgba(8,145,178,0.8)]" />
                )}
              </Link>
            </div>
          )
        })}

        {/* Separator */}
        <div className="mx-1.5 h-5 w-px shrink-0 bg-border/20" />

        {/* Command palette trigger */}
        <button
          onClick={onCommandPalette}
          className="dock-item flex h-9 shrink-0 items-center gap-1.5 rounded-lg px-2.5 text-muted-foreground/30 transition-all duration-200 hover:text-foreground/60"
          aria-label="Open command palette"
          title="Command Palette (⌘K)"
        >
          <Command className="h-3.5 w-3.5" />
          <span className="hidden font-mono text-[8px] tracking-widest sm:inline">⌘K</span>
        </button>
      </div>
    </nav>
  )
}

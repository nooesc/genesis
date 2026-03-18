import { useEffect } from 'react'
import { useNavigate } from '@tanstack/react-router'
import {
  LayoutDashboard, MessagesSquare, Brain, Database,
  Clock, Wrench, BarChart3, FileText, Settings,
} from 'lucide-react'
import {
  CommandDialog,
  Command,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
} from '@/components/ui/command'

const routes = [
  { to: '/', label: 'Dashboard', icon: LayoutDashboard, keywords: 'home overview kpi' },
  { to: '/sessions', label: 'Sessions', icon: MessagesSquare, keywords: 'chat conversations messages' },
  { to: '/skills', label: 'Skills', icon: Brain, keywords: 'abilities prompts templates' },
  { to: '/memories', label: 'Memories', icon: Database, keywords: 'knowledge storage recall' },
  { to: '/schedules', label: 'Schedules', icon: Clock, keywords: 'cron timer jobs automation' },
  { to: '/tools', label: 'Tools', icon: Wrench, keywords: 'functions registry mcp' },
  { to: '/analytics', label: 'Analytics', icon: BarChart3, keywords: 'stats usage tokens charts' },
  { to: '/audit', label: 'Audit Log', icon: FileText, keywords: 'history actions log events' },
  { to: '/settings', label: 'Settings', icon: Settings, keywords: 'config preferences' },
] as const

interface CommandPaletteProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function CommandPalette({ open, onOpenChange }: CommandPaletteProps) {
  const navigate = useNavigate()

  // Cmd+K / Ctrl+K shortcut
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        onOpenChange(!open)
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [open, onOpenChange])

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <Command className="border-none bg-[#0d0d0d]">
        <CommandInput placeholder="Navigate to..." className="font-mono text-xs" />
        <CommandList>
          <CommandEmpty className="font-mono text-xs text-muted-foreground">
            No results found.
          </CommandEmpty>
          <CommandGroup heading="Navigation">
            {routes.map(({ to, label, icon: Icon, keywords }) => (
              <CommandItem
                key={to}
                value={`${label} ${keywords}`}
                onSelect={() => {
                  navigate({ to })
                  onOpenChange(false)
                }}
                className="gap-3 py-2"
              >
                <Icon className="h-4 w-4 text-muted-foreground" />
                <span className="font-mono text-xs">{label}</span>
                <span className="ml-auto font-mono text-[9px] text-muted-foreground/40">
                  {to}
                </span>
              </CommandItem>
            ))}
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  )
}

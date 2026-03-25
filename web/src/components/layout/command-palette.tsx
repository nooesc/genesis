import { useNavigate } from '@tanstack/react-router'
import { useQueryClient } from '@tanstack/react-query'
import { useMemo } from 'react'
import { toast } from 'sonner'
import { navRoutes } from '@/lib/nav'
import { api } from '@/lib/api/client'
import { useHealth } from '@/lib/api/queries/health'
import { useInsights } from '@/lib/api/queries/analytics'
import { useAuditLog } from '@/lib/api/queries/audit'
import { useSchedules } from '@/lib/api/queries/schedules'
import { useSessions } from '@/lib/api/queries/sessions'
import { buildCommandMapModel } from '@/lib/command-map/selectors'
import { buildCommandMapJumpTarget, buildCommandMapSearchIndex } from '@/lib/command-map/search'
import type { CommandMapProjectionInput } from '@/lib/command-map/types'
import {
  PlusIcon, Trash2Icon, RefreshCwIcon, DownloadIcon, Radar,
} from 'lucide-react'
import {
  CommandDialog,
  Command,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
  CommandSeparator,
} from '@/components/ui/command'

interface CommandPaletteProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

interface ActionCommand {
  label: string
  keywords: string
  icon: typeof PlusIcon
  action: (ctx: { navigate: ReturnType<typeof useNavigate>; queryClient: ReturnType<typeof useQueryClient>; close: () => void }) => void
}

const actions: ActionCommand[] = [
  {
    label: 'New Skill',
    keywords: 'create add skill',
    icon: PlusIcon,
    action: ({ navigate, close }) => {
      navigate({ to: '/skills' })
      close()
      // The skills page has its own "New" button — navigate there
    },
  },
  {
    label: 'New Schedule',
    keywords: 'create add schedule cron job',
    icon: PlusIcon,
    action: ({ navigate, close }) => {
      navigate({ to: '/schedules' })
      close()
    },
  },
  {
    label: 'Purge Old Sessions',
    keywords: 'delete clean sessions purge',
    icon: Trash2Icon,
    action: async ({ queryClient, close }) => {
      close()
      try {
        await api.delete<void>('/sessions/purge?older_than_days=30')
        queryClient.invalidateQueries({ queryKey: ['sessions'] })
        toast.success('Purged sessions older than 30 days')
      } catch (err) {
        toast.error(`Purge failed: ${err instanceof Error ? err.message : 'Unknown error'}`)
      }
    },
  },
  {
    label: 'Refresh All Data',
    keywords: 'reload refetch invalidate cache',
    icon: RefreshCwIcon,
    action: ({ queryClient, close }) => {
      queryClient.invalidateQueries()
      toast.success('Refreshing all data')
      close()
    },
  },
  {
    label: 'Export Health Report',
    keywords: 'download health status json',
    icon: DownloadIcon,
    action: async ({ close }) => {
      close()
      try {
        const health = await api.get<Record<string, unknown>>('/health')
        const blob = new Blob([JSON.stringify(health, null, 2)], { type: 'application/json' })
        const url = URL.createObjectURL(blob)
        const a = document.createElement('a')
        a.href = url
        a.download = `genesis-health-${new Date().toISOString().slice(0, 10)}.json`
        a.click()
        URL.revokeObjectURL(url)
        toast.success('Health report downloaded')
      } catch (err) {
        toast.error(`Export failed: ${err instanceof Error ? err.message : 'Unknown error'}`)
      }
    },
  },
]

export function CommandPalette({ open, onOpenChange }: CommandPaletteProps) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const { data: health } = useHealth({ enabled: open })
  const { data: insights } = useInsights(7, { enabled: open, refetchInterval: 60_000 })
  const { data: sessions } = useSessions({ limit: 20 }, { enabled: open })
  const { data: schedules } = useSchedules({ enabled: open })
  const { data: audit = [] } = useAuditLog({ limit: 24 }, { enabled: open, refetchInterval: 30_000 })

  const close = () => onOpenChange(false)
  const ctx = { navigate, queryClient, close }
  const commandMapIndex = useMemo(() => {
    if (!open) return []

    const input: CommandMapProjectionInput = {
      health: health ?? null,
      sessions: sessions ?? [],
      schedules: schedules ?? [],
      audit,
      insights: insights ?? null,
    }

    return buildCommandMapSearchIndex(buildCommandMapModel(input))
  }, [audit, health, insights, open, schedules, sessions])

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <Command className="border-none bg-[#0d0d0d]">
        <CommandInput placeholder="Search commands..." className="font-mono text-xs" />
        <CommandList>
          <CommandEmpty className="font-mono text-xs text-muted-foreground">
            No results found.
          </CommandEmpty>
          <CommandGroup heading="Navigation">
            {navRoutes.map(({ to, label, icon: Icon, keywords, shortcut }) => (
              <CommandItem
                key={to}
                value={`${label} ${keywords}`}
                onSelect={() => {
                  navigate({ to })
                  close()
                }}
                className="gap-3 py-2"
              >
                <Icon className="h-4 w-4 text-muted-foreground" />
                <span className="font-mono text-xs">{label}</span>
                <div className="ml-auto flex items-center gap-2">
                  {shortcut && (
                    <kbd className="flex h-4 min-w-[16px] items-center justify-center rounded border border-border/30 bg-muted/20 px-1 font-mono text-[8px] text-muted-foreground/40">
                      {shortcut}
                    </kbd>
                  )}
                  <span className="font-mono text-[9px] text-muted-foreground/30">
                    {to}
                  </span>
                </div>
              </CommandItem>
            ))}
          </CommandGroup>
          {commandMapIndex.length > 0 && (
            <>
              <CommandSeparator />
              <CommandGroup heading="Command Map">
                {commandMapIndex.map((entry) => (
                  <CommandItem
                    key={entry.nodeId}
                    value={`${entry.label} ${entry.searchText}`}
                    onSelect={() => {
                      navigate(buildCommandMapJumpTarget(entry.nodeId))
                      close()
                    }}
                    className="gap-3 py-2"
                  >
                    <Radar className="h-4 w-4 text-muted-foreground" />
                    <div className="flex min-w-0 flex-col">
                      <span className="font-mono text-xs">{entry.label}</span>
                      <span className="truncate font-mono text-[10px] text-muted-foreground/40">
                        {entry.kind} · {entry.layer}
                        {entry.subtitle ? ` · ${entry.subtitle}` : ''}
                      </span>
                    </div>
                  </CommandItem>
                ))}
              </CommandGroup>
            </>
          )}
          <CommandSeparator />
          <CommandGroup heading="Actions">
            {actions.map((cmd) => (
              <CommandItem
                key={cmd.label}
                value={`${cmd.label} ${cmd.keywords}`}
                onSelect={() => cmd.action(ctx)}
                className="gap-3 py-2"
              >
                <cmd.icon className="h-4 w-4 text-muted-foreground" />
                <span className="font-mono text-xs">{cmd.label}</span>
              </CommandItem>
            ))}
          </CommandGroup>
        </CommandList>
      </Command>
    </CommandDialog>
  )
}

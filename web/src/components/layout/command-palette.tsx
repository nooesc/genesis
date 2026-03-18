import { useCallback, useEffect } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { navRoutes } from '@/lib/nav'
import {
  CommandDialog,
  Command,
  CommandInput,
  CommandList,
  CommandEmpty,
  CommandGroup,
  CommandItem,
} from '@/components/ui/command'

interface CommandPaletteProps {
  open: boolean
  onOpenChange: (open: boolean) => void
}

export function CommandPalette({ open, onOpenChange }: CommandPaletteProps) {
  const navigate = useNavigate()

  const toggleOpen = useCallback(() => {
    onOpenChange(!open)
  }, [open, onOpenChange])

  // Cmd+K / Ctrl+K shortcut
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        toggleOpen()
      }
    }
    document.addEventListener('keydown', handleKeyDown)
    return () => document.removeEventListener('keydown', handleKeyDown)
  }, [toggleOpen])

  return (
    <CommandDialog open={open} onOpenChange={onOpenChange}>
      <Command className="border-none bg-[#0d0d0d]">
        <CommandInput placeholder="Navigate to..." className="font-mono text-xs" />
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
                  onOpenChange(false)
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
        </CommandList>
      </Command>
    </CommandDialog>
  )
}

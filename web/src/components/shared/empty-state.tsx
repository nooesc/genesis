import { InboxIcon } from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

interface EmptyStateProps {
  title: string
  description?: string
  icon?: LucideIcon
}

export function EmptyState({ title, description, icon: Icon = InboxIcon }: EmptyStateProps) {
  return (
    <div className="flex flex-col items-center justify-center py-16 text-center">
      <div className="mb-3 flex h-10 w-10 items-center justify-center rounded-full bg-muted/30 ring-1 ring-border/20">
        <Icon className="h-4 w-4 text-muted-foreground/30" />
      </div>
      <p className="font-mono text-[11px] font-medium text-muted-foreground/60">{title}</p>
      {description && (
        <p className="mt-1 max-w-xs font-mono text-[10px] text-muted-foreground/30">{description}</p>
      )}
    </div>
  )
}

import type { LucideIcon } from 'lucide-react'

interface PageHeaderProps {
  title: string
  icon?: LucideIcon
  count?: number | string
  children?: React.ReactNode
}

export function PageHeader({ title, icon: Icon, count, children }: PageHeaderProps) {
  return (
    <div className="flex items-center justify-between gap-3">
      <div className="flex items-center gap-3">
        {Icon && (
          <div className="flex h-6 w-6 items-center justify-center rounded-md bg-primary/5 ring-1 ring-primary/10">
            <Icon className="h-3 w-3 text-primary/60" />
          </div>
        )}
        <h1 className="title-gradient font-mono text-sm font-semibold uppercase tracking-[0.15em]">
          {title}
        </h1>
        {count != null && (
          <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
            {count}
          </span>
        )}
      </div>
      {children}
    </div>
  )
}

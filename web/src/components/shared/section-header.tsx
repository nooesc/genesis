export function SectionHeader({ title }: { title: string }) {
  return (
    <div className="flex items-center gap-2">
      <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/50">
        {title}
      </span>
      <div className="section-divider h-px flex-1 opacity-20" />
    </div>
  )
}

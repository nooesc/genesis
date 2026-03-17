export function EmptyState({ title, description }: { title: string; description?: string }) {
  return (
    <div className="flex flex-col items-center justify-center py-12 text-center">
      <p className="font-mono text-sm text-muted-foreground">{title}</p>
      {description && <p className="mt-1 text-xs text-muted-foreground/60">{description}</p>}
    </div>
  )
}

interface ToolHeatmapProps {
  /** [tool_name, count] tuples from API */
  toolUsage: [string, number][]
  maxItems?: number
}

function getHeatColor(intensity: number): string {
  if (intensity > 0.8) return 'bg-primary/80'
  if (intensity > 0.6) return 'bg-primary/60'
  if (intensity > 0.4) return 'bg-primary/40'
  if (intensity > 0.2) return 'bg-primary/25'
  return 'bg-primary/10'
}

export function ToolHeatmap({ toolUsage, maxItems = 24 }: ToolHeatmapProps) {
  const entries = [...toolUsage]
    .sort(([, a], [, b]) => b - a)
    .slice(0, maxItems)

  if (entries.length === 0) {
    return (
      <div className="flex h-full items-center justify-center font-mono text-xs text-muted-foreground/50">
        No tool usage data
      </div>
    )
  }

  const maxCount = entries[0][1]

  return (
    <div className="grid grid-cols-6 gap-1">
      {entries.map(([name, count]) => {
        const intensity = maxCount > 0 ? count / maxCount : 0
        return (
          <div
            key={name}
            className={`group relative flex h-8 items-center justify-center rounded ${getHeatColor(intensity)} transition-colors duration-200 hover:ring-1 hover:ring-primary/50`}
          >
            <span className="truncate px-1 font-mono text-[8px] text-foreground/60">
              {name.replace(/_/g, ' ').slice(0, 8)}
            </span>
            {/* Hover tooltip */}
            <div className="pointer-events-none absolute -top-7 left-1/2 z-10 hidden -translate-x-1/2 whitespace-nowrap rounded bg-card px-2 py-0.5 font-mono text-[10px] text-foreground shadow-lg ring-1 ring-border/50 group-hover:block">
              {name}: {count}
            </div>
          </div>
        )
      })}
    </div>
  )
}

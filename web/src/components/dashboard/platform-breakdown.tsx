interface PlatformBreakdownProps {
  data: [string, number][]
}

export function PlatformBreakdown({ data }: PlatformBreakdownProps) {
  const total = data.reduce((sum, [, count]) => sum + count, 0)

  if (data.length === 0 || total === 0) {
    return (
      <div className="flex h-full items-center justify-center font-mono text-xs text-muted-foreground">
        No platform data
      </div>
    )
  }

  const sorted = [...data].sort(([, a], [, b]) => b - a)

  return (
    <div className="flex flex-col gap-3">
      {sorted.map(([platform, count]) => {
        const pct = Math.round((count / total) * 100)
        return (
          <div key={platform} className="flex flex-col gap-1">
            <div className="flex items-center justify-between">
              <span className="font-mono text-xs capitalize text-foreground">{platform}</span>
              <span className="font-mono text-xs text-muted-foreground">
                {count} ({pct}%)
              </span>
            </div>
            <div className="h-1.5 w-full overflow-hidden rounded-full bg-muted">
              <div
                className="h-full rounded-full bg-primary transition-all"
                style={{ width: `${pct}%` }}
              />
            </div>
          </div>
        )
      })}
    </div>
  )
}

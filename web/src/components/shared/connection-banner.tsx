import { useHealth } from '@/lib/api/queries/health'

export function ConnectionBanner() {
  const { isError } = useHealth()
  if (!isError) return null
  return (
    <div className="flex items-center justify-center gap-2 bg-destructive/10 px-4 py-1.5">
      <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-destructive" />
      <span className="font-mono text-[10px] uppercase tracking-wider text-destructive/80">
        Connection lost — retrying
      </span>
    </div>
  )
}

import { useHealth } from '@/lib/api/queries/health'

export function ConnectionBanner() {
  const { isError } = useHealth()
  if (!isError) return null
  return (
    <div className="flex items-center justify-center gap-2 bg-destructive/10 px-4 py-2 text-sm text-destructive">
      <div className="h-2 w-2 rounded-full bg-destructive" />
      Connection lost — retrying...
    </div>
  )
}

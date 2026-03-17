import { useRouterState } from '@tanstack/react-router'

export function Topbar() {
  const router = useRouterState()
  const pathname = router.location.pathname
  const title =
    pathname === '/'
      ? 'Dashboard'
      : pathname.split('/').filter(Boolean)[0]?.replace(/^\w/, c => c.toUpperCase()) ?? 'Dashboard'

  return (
    <header className="flex h-14 items-center justify-between border-b border-border px-6">
      <h1 className="text-sm font-medium">{title}</h1>
      <div className="flex items-center gap-2 text-xs text-muted-foreground">
        <div className="h-2 w-2 rounded-full bg-green-500" />
        Connected
      </div>
    </header>
  )
}

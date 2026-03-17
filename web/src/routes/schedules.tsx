import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/schedules')({
  component: () => <div className="font-mono text-muted-foreground">Schedules — coming soon</div>,
})

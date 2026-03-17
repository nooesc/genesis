import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/analytics')({
  component: () => <div className="font-mono text-muted-foreground">Analytics — coming soon</div>,
})

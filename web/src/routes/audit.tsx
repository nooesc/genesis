import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/audit')({
  component: () => <div className="font-mono text-muted-foreground">Audit Log — coming soon</div>,
})

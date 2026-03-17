import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/sessions/$id')({
  component: () => <div className="font-mono text-muted-foreground">Session detail — coming soon</div>,
})

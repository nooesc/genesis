import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/skills')({
  component: () => <div className="font-mono text-muted-foreground">Skills — coming soon</div>,
})

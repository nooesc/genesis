import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/memories')({
  component: () => <div className="font-mono text-muted-foreground">Memories — coming soon</div>,
})

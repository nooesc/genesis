import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/settings')({
  component: () => <div className="font-mono text-muted-foreground">Settings — coming soon</div>,
})

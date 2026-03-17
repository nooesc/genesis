import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/sessions/')({
  component: () => <div className="font-mono text-muted-foreground">Sessions — coming soon</div>,
})

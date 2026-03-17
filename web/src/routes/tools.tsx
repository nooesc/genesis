import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/tools')({
  component: () => <div className="font-mono text-muted-foreground">Tools — coming soon</div>,
})

import { createLazyFileRoute, Link } from '@tanstack/react-router'
import { ChevronLeftIcon } from 'lucide-react'
import { useSession, useMessages } from '@/lib/api/queries/sessions'
import { SessionHeader } from '@/components/sessions/session-header'
import { MessageThread } from '@/components/sessions/message-thread'
import { Skeleton } from '@/components/ui/skeleton'

export const Route = createLazyFileRoute('/sessions/$id')({
  component: SessionDetailPage,
})

function SessionDetailPage() {
  const { id } = Route.useParams()
  const { data: session, isLoading: sessionLoading } = useSession(id)
  const { data: messages, isLoading: messagesLoading } = useMessages(id)

  const isLoading = sessionLoading || messagesLoading

  return (
    <div className="flex flex-col gap-4">
      <Link
        to="/sessions"
        className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronLeftIcon className="size-3" />
        Sessions
      </Link>

      {isLoading ? (
        <div className="flex flex-col gap-4">
          <Skeleton className="h-24 w-full rounded-xl" />
          <div className="flex flex-col gap-3">
            {Array.from({ length: 6 }).map((_, i) => (
              <Skeleton key={i} className="h-16 w-full rounded" />
            ))}
          </div>
        </div>
      ) : session ? (
        <>
          <SessionHeader session={session} messageCount={messages?.length} />
          <MessageThread messages={messages ?? []} />
        </>
      ) : (
        <div className="flex items-center justify-center py-16">
          <p className="font-mono text-xs text-muted-foreground">Session not found.</p>
        </div>
      )}
    </div>
  )
}

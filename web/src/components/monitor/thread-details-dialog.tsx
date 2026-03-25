import { useNavigate } from '@tanstack/react-router'
import { Badge } from '@/components/ui/badge'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Button } from '@/components/ui/button'
import { useMessages, useSession } from '@/lib/api/queries/sessions'

interface ThreadDetailsDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  sessionId: string | null
}

export function ThreadDetailsDialog({ open, onOpenChange, sessionId }: ThreadDetailsDialogProps) {
  const navigate = useNavigate()
  const targetSessionId = open ? sessionId ?? '' : ''
  const { data: session, isLoading: sessionLoading } = useSession(targetSessionId)
  const { data: messages, isLoading: messagesLoading } = useMessages(targetSessionId)

  if (!sessionId) return null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Thread details</DialogTitle>
          <DialogDescription className="font-mono text-xs">
            Inspect the conversation and open the full session page if needed.
          </DialogDescription>
        </DialogHeader>

        {sessionLoading ? (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Loading thread...
          </div>
        ) : session ? (
          <div className="space-y-3">
            <div className="space-y-1">
              <h2 className="font-mono text-base font-semibold text-foreground">{session.title ?? session.id}</h2>
              <p className="font-mono text-xs text-muted-foreground/70">
                {session.platform} · {session.total_input_tokens + session.total_output_tokens} tokens
              </p>
            </div>

            <div className="flex flex-wrap gap-2">
              <Badge variant="secondary" className="font-mono text-[10px] uppercase tracking-[0.18em]">
                session {session.id}
              </Badge>
              {session.parent_session_id && (
                <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
                  parent {session.parent_session_id}
                </Badge>
              )}
            </div>

            <div className="space-y-2">
              {(messages ?? []).map(message => (
                <div key={message.id} className="rounded-lg border border-border/20 bg-muted/20 p-3">
                  <div className="mb-1 flex items-center justify-between gap-2 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-foreground/60">
                    <span>{message.role}</span>
                    <span>{message.created_at}</span>
                  </div>
                  <pre className="whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-foreground/80">
                    {message.content ?? '(empty)'}
                  </pre>
                </div>
              ))}
              {messagesLoading && (
                <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
                  Loading messages...
                </div>
              )}
              {!messagesLoading && (messages ?? []).length === 0 && (
                <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
                  No messages found.
                </div>
              )}
            </div>
          </div>
        ) : (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Thread not found.
          </div>
        )}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => navigate({ to: '/sessions/$id', params: { id: sessionId } })}
            className="font-mono text-[11px] uppercase tracking-[0.18em]"
          >
            Open session
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

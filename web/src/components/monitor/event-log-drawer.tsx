import { Badge } from '@/components/ui/badge'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { useAuditLog } from '@/lib/api/queries/audit'

interface EventLogDrawerProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  sessionId?: string | null
  eventType?: string | null
}

export function EventLogDrawer({ open, onOpenChange, title, sessionId = null, eventType = null }: EventLogDrawerProps) {
  const { data: auditEntries = [] } = useAuditLog({ limit: 50 }, { enabled: open, refetchInterval: 30_000 })

  const entries = auditEntries.filter(entry => {
    if (sessionId && entry.session_id !== sessionId) return false
    if (eventType && entry.event_type !== eventType) return false
    return true
  })

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent side="right" className="w-full sm:max-w-xl">
        <SheetHeader>
          <SheetTitle className="font-mono text-sm">Event log</SheetTitle>
          <SheetDescription className="font-mono text-xs">
            {title}
          </SheetDescription>
        </SheetHeader>

        <div className="flex flex-wrap gap-2 px-4">
          {sessionId && (
            <Badge variant="secondary" className="font-mono text-[10px] uppercase tracking-[0.18em]">
              session {sessionId}
            </Badge>
          )}
          {eventType && (
            <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
              {eventType}
            </Badge>
          )}
        </div>

        <div className="flex flex-1 flex-col gap-2 overflow-auto px-4 pb-4">
          {entries.length === 0 ? (
            <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
              No matching events found.
            </div>
          ) : (
            entries.map(entry => (
              <div key={entry.id} className="rounded-lg border border-border/20 bg-muted/20 p-3">
                <div className="mb-1 flex items-center justify-between gap-2 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-foreground/60">
                  <span>{entry.event_type}</span>
                  <span>{entry.created_at}</span>
                </div>
                <div className="mb-1 font-mono text-[10px] uppercase tracking-[0.18em] text-muted-foreground/60">
                  {entry.session_id ? `session ${entry.session_id}` : 'global'}
                </div>
                <pre className="whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-foreground/80">
                  {entry.details ?? '(no details)'}
                </pre>
              </div>
            ))
          )}
        </div>
      </SheetContent>
    </Sheet>
  )
}

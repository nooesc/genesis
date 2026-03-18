import * as React from 'react'
import { useNavigate } from '@tanstack/react-router'
import { GitForkIcon, Trash2Icon, Copy, DownloadIcon } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { useDeleteSession, useForkSession } from '@/lib/api/mutations/sessions'
import type { SessionSummary } from '@/lib/api/types'
import { formatTokens } from '@/lib/utils'

interface SessionHeaderProps {
  session: SessionSummary
  messageCount?: number
}

function formatAge(isoString: string): string {
  const ms = Date.now() - new Date(isoString).getTime()
  const sec = Math.floor(ms / 1000)
  if (sec < 60) return `${sec}s ago`
  const min = Math.floor(sec / 60)
  if (min < 60) return `${min}m ago`
  const hr = Math.floor(min / 60)
  if (hr < 24) return `${hr}h ago`
  return `${Math.floor(hr / 24)}d ago`
}

export function SessionHeader({ session, messageCount }: SessionHeaderProps) {
  const navigate = useNavigate()
  const [deleteOpen, setDeleteOpen] = React.useState(false)
  const [copied, setCopied] = React.useState(false)
  const deleteMutation = useDeleteSession()
  const forkMutation = useForkSession()

  const totalTokens = session.total_input_tokens + session.total_output_tokens

  function handleDelete() {
    deleteMutation.mutate(session.id, {
      onSuccess: () => {
        setDeleteOpen(false)
        void navigate({ to: '/sessions' })
      },
    })
  }

  function handleFork() {
    forkMutation.mutate(session.id, {
      onSuccess: (forked) => {
        void navigate({ to: '/sessions/$id', params: { id: forked.id } })
      },
    })
  }

  function handleCopyId() {
    navigator.clipboard.writeText(session.id).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    }).catch(() => {
      // clipboard unavailable (non-HTTPS or denied)
    })
  }

  return (
    <div className="rounded-lg border border-border/40 bg-card/30 p-4">
      {/* Top: Title + Actions */}
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0 flex-1">
          <h1 className="truncate font-mono text-sm font-semibold text-foreground">
            {session.title ?? (
              <span className="italic text-muted-foreground/50">Untitled Session</span>
            )}
          </h1>
          <div className="mt-1 flex items-center gap-2">
            <button
              onClick={handleCopyId}
              className="flex items-center gap-1 font-mono text-[10px] text-muted-foreground/40 transition-colors hover:text-muted-foreground"
              title="Copy session ID"
            >
              <Copy className="h-2.5 w-2.5" />
              {copied ? 'copied' : session.id.slice(0, 12)}
            </button>
            <Badge variant="outline" className="font-mono text-[9px] uppercase">
              {session.platform}
            </Badge>
            {session.parent_session_id && (
              <span className="font-mono text-[9px] text-muted-foreground/30">
                fork of {session.parent_session_id.slice(0, 8)}
              </span>
            )}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1.5">
          <Button variant="outline" size="sm" onClick={handleFork} disabled={forkMutation.isPending} className="h-7 px-2 font-mono text-[10px]">
            <GitForkIcon className="mr-1 h-3 w-3" />
            Fork
          </Button>
          <Button variant="outline" size="sm" asChild className="h-7 px-2 font-mono text-[10px]">
            <a href={`/api/sessions/${session.id}/export`} download>
              <DownloadIcon className="mr-1 h-3 w-3" />
              Export
            </a>
          </Button>
          <Dialog open={deleteOpen} onOpenChange={setDeleteOpen}>
            <DialogTrigger asChild>
              <Button variant="outline" size="sm" className="h-7 px-2 font-mono text-[10px] text-destructive hover:bg-destructive/10">
                <Trash2Icon className="mr-1 h-3 w-3" />
                Delete
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Delete session?</DialogTitle>
                <DialogDescription>
                  This will permanently delete the session and all its messages.
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button variant="destructive" size="sm" onClick={handleDelete} disabled={deleteMutation.isPending}>
                  {deleteMutation.isPending ? 'Deleting…' : 'Confirm Delete'}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>
      </div>

      {/* Bottom: Metrics strip */}
      <div className="mt-3 flex items-center gap-1 border-t border-border/20 pt-3">
        <MetricChip label="TURNS" value={messageCount != null ? String(messageCount) : '—'} />
        <ChipDivider />
        <MetricChip label="TOKENS" value={formatTokens(totalTokens)} />
        <ChipDivider />
        <MetricChip label="IN" value={formatTokens(session.total_input_tokens)} dim />
        <ChipDivider />
        <MetricChip label="OUT" value={formatTokens(session.total_output_tokens)} dim />
        <ChipDivider />
        <MetricChip label="CREATED" value={formatAge(session.created_at)} dim />
        <ChipDivider />
        <MetricChip label="UPDATED" value={formatAge(session.updated_at)} dim />
      </div>
    </div>
  )
}

function MetricChip({ label, value, dim }: { label: string; value: string; dim?: boolean }) {
  return (
    <div className="flex items-center gap-1.5 px-2">
      <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/40">
        {label}
      </span>
      <span className={`font-mono text-[11px] tabular-nums ${dim ? 'text-foreground/50' : 'text-foreground/80'}`}>
        {value}
      </span>
    </div>
  )
}

function ChipDivider() {
  return <div className="h-3 w-px bg-border/20" />
}

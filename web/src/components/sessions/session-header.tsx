import * as React from 'react'
import { useNavigate } from '@tanstack/react-router'
import { DownloadIcon, GitForkIcon, Trash2Icon } from 'lucide-react'
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

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K`
  return String(n)
}

interface SessionHeaderProps {
  session: SessionSummary
  messageCount?: number
}

export function SessionHeader({ session, messageCount }: SessionHeaderProps) {
  const navigate = useNavigate()
  const [deleteOpen, setDeleteOpen] = React.useState(false)
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

  return (
    <div className="flex flex-col gap-3 border-b border-border pb-4">
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h1 className="font-mono text-base font-semibold">
            {session.title ?? (
              <span className="italic text-muted-foreground">Untitled</span>
            )}
          </h1>
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-[10px] text-muted-foreground">
              {session.id}
            </span>
            <Badge variant="outline" className="font-mono text-[10px]">
              {session.platform}
            </Badge>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={handleFork}
            disabled={forkMutation.isPending}
          >
            <GitForkIcon className="mr-1" />
            Fork
          </Button>

          <Button variant="outline" size="sm" asChild>
            <a href={`/api/sessions/${session.id}/export`} download>
              <DownloadIcon className="mr-1" />
              Export
            </a>
          </Button>

          <Dialog open={deleteOpen} onOpenChange={setDeleteOpen}>
            <DialogTrigger asChild>
              <Button variant="destructive" size="sm">
                <Trash2Icon className="mr-1" />
                Delete
              </Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Delete session?</DialogTitle>
                <DialogDescription>
                  This will permanently delete the session and all its messages. This action
                  cannot be undone.
                </DialogDescription>
              </DialogHeader>
              <DialogFooter>
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={handleDelete}
                  disabled={deleteMutation.isPending}
                >
                  {deleteMutation.isPending ? 'Deleting…' : 'Delete'}
                </Button>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </div>
      </div>

      <div className="flex flex-wrap gap-4">
        <Stat label="Turns" value={messageCount != null ? String(messageCount) : '—'} />
        <Stat label="Tokens" value={formatTokens(totalTokens)} />
        <Stat
          label="Input"
          value={formatTokens(session.total_input_tokens)}
          sub="tokens"
        />
        <Stat
          label="Output"
          value={formatTokens(session.total_output_tokens)}
          sub="tokens"
        />
        {session.parent_session_id && (
          <div className="flex flex-col gap-0.5">
            <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/60">
              Forked from
            </span>
            <span className="font-mono text-xs text-muted-foreground">
              {session.parent_session_id.slice(0, 8)}
            </span>
          </div>
        )}
      </div>
    </div>
  )
}

function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground/60">
        {label}
      </span>
      <span className="font-mono text-sm font-medium">
        {value}
        {sub && <span className="ml-1 text-[10px] text-muted-foreground/60">{sub}</span>}
      </span>
    </div>
  )
}

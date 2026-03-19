import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import { PlusIcon, Trash2Icon, Clock, Play, Pause } from 'lucide-react'
import { toast } from 'sonner'
import { useSchedules } from '@/lib/api/queries/schedules'
import {
  useCreateSchedule,
  useDeleteSchedule,
  useToggleSchedule,
} from '@/lib/api/mutations/schedules'
import { EmptyState } from '@/components/shared/empty-state'
import { ConfirmDeleteDialog } from '@/components/shared/confirm-delete-dialog'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Textarea } from '@/components/ui/textarea'
import { Badge } from '@/components/ui/badge'
import type { Schedule } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'

export const Route = createLazyFileRoute('/schedules')({
  component: SchedulesPage,
})

function describeCron(expr: string): string {
  const parts = expr.trim().split(/\s+/)
  if (parts.length !== 5) return expr

  const [min, hour, dom, mon, dow] = parts

  if (min.startsWith('*/') && hour === '*') {
    return `Every ${min.slice(2)} min`
  }
  if (hour.startsWith('*/')) {
    return `Every ${hour.slice(2)} hr`
  }
  if (min !== '*' && hour !== '*' && !hour.includes('/') && dom === '*' && mon === '*' && dow === '*') {
    return `Daily at ${hour.padStart(2, '0')}:${min.padStart(2, '0')}`
  }
  if (dom === '*' && mon === '*' && dow !== '*') {
    const label = dow === '1-5' ? 'Weekdays' : `DOW(${dow})`
    return `${label} at ${hour.padStart(2, '0')}:${min.padStart(2, '0')}`
  }
  return expr
}

function ScheduleCard({
  schedule,
  onToggle,
  onDelete,
  togglePending,
}: {
  schedule: Schedule
  onToggle: () => void
  onDelete: () => void
  togglePending: boolean
}) {
  return (
    <div className={`group rounded-md border p-3 transition-all duration-200 ${
      schedule.enabled
        ? 'border-border/40 bg-card/30 hover:border-border/50 hover:-translate-y-px hover:shadow-[0_2px_8px_rgba(0,0,0,0.25)]'
        : 'border-border/20 bg-card/10 opacity-60'
    }`}>
      {/* Header: ID + status + actions */}
      <div className="mb-2 flex items-center gap-2">
        <Clock className="h-3 w-3 text-muted-foreground/40" />
        <span className="font-mono text-[11px] font-medium text-foreground/80">
          {schedule.id}
        </span>
        <Badge
          variant={schedule.enabled ? 'secondary' : 'outline'}
          className="ml-auto font-mono text-[9px] uppercase"
        >
          {schedule.enabled ? 'active' : 'paused'}
        </Badge>
        <button
          onClick={onToggle}
          disabled={togglePending}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground/40 transition-colors hover:text-foreground"
          title={schedule.enabled ? 'Pause' : 'Resume'}
        >
          {schedule.enabled ? <Pause className="h-3 w-3" /> : <Play className="h-3 w-3" />}
        </button>
        <button
          onClick={onDelete}
          className="flex h-6 w-6 items-center justify-center rounded text-muted-foreground/40 opacity-0 transition-all hover:text-destructive group-hover:opacity-100 group-focus-within:opacity-100 focus:opacity-100"
          title="Delete"
        >
          <Trash2Icon className="h-3 w-3" />
        </button>
      </div>

      {/* Cron + human-readable */}
      <div className="mb-2 flex items-center gap-2">
        <code className="rounded bg-muted/30 px-1.5 py-0.5 font-mono text-[10px] text-primary/80">
          {schedule.cron_expression}
        </code>
        <span className="font-mono text-[9px] text-muted-foreground/40">
          {describeCron(schedule.cron_expression)}
        </span>
      </div>

      {/* Destination */}
      <div className="mb-1.5 flex items-center gap-1.5">
        <span className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
          TO
        </span>
        <span className="font-mono text-[10px] text-muted-foreground/60">
          {schedule.destination}
        </span>
      </div>

      {/* Prompt preview */}
      <p className="mb-1.5 line-clamp-2 font-mono text-[10px] leading-relaxed text-foreground/50">
        {schedule.prompt}
      </p>

      {/* Last run */}
      {schedule.last_run_at && (
        <span className="font-mono text-[9px] text-muted-foreground/30">
          Last run {formatRelativeTime(schedule.last_run_at)}
        </span>
      )}
    </div>
  )
}

function NewScheduleDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const createSchedule = useCreateSchedule()
  const [id, setId] = React.useState('')
  const [cronExpression, setCronExpression] = React.useState('')
  const [destination, setDestination] = React.useState('')
  const [prompt, setPrompt] = React.useState('')

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (!cronExpression.trim() || !destination.trim() || !prompt.trim()) return
    createSchedule.mutate(
      {
        id: id.trim() || undefined,
        cron_expression: cronExpression.trim(),
        destination: destination.trim(),
        prompt: prompt.trim(),
      },
      {
        onSuccess: () => {
          toast.success('Schedule created')
          handleClose()
        },
        onError: (err) => {
          toast.error(`Failed to create: ${err.message}`)
        },
      },
    )
  }

  function handleClose() {
    setId('')
    setCronExpression('')
    setDestination('')
    setPrompt('')
    onClose()
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && handleClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">New Schedule</DialogTitle>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">ID (optional)</Label>
            <Input
              value={id}
              onChange={(e) => setId(e.target.value)}
              placeholder="my-daily-report"
              className="font-mono text-xs"
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Cron Expression</Label>
            <Input
              value={cronExpression}
              onChange={(e) => setCronExpression(e.target.value)}
              placeholder="0 9 * * *"
              className="font-mono text-xs"
              required
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Destination</Label>
            <Input
              value={destination}
              onChange={(e) => setDestination(e.target.value)}
              placeholder="cli or platform channel"
              className="font-mono text-xs"
              required
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Prompt</Label>
            <Textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What should the agent do..."
              className="font-mono text-xs"
              rows={4}
              required
            />
          </div>
          <DialogFooter className="-mx-0 -mb-0 border-0 bg-transparent p-0 pt-1">
            <Button type="button" variant="outline" size="sm" onClick={handleClose}>
              Cancel
            </Button>
            <Button
              type="submit"
              size="sm"
              disabled={
                createSchedule.isPending ||
                !cronExpression.trim() ||
                !destination.trim() ||
                !prompt.trim()
              }
            >
              {createSchedule.isPending ? 'Creating...' : 'Create'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function SchedulesPage() {
  const { data: schedules, isLoading } = useSchedules()
  const toggleSchedule = useToggleSchedule()
  const deleteSchedule = useDeleteSchedule()
  const [newOpen, setNewOpen] = React.useState(false)
  const [deleteTarget, setDeleteTarget] = React.useState<Schedule | null>(null)

  const activeCount = (schedules ?? []).filter(s => s.enabled).length
  const totalCount = schedules?.length ?? 0

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Schedules
          </h1>
          {!isLoading && (
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
              {activeCount}/{totalCount} active
            </span>
          )}
        </div>
        <Button
          size="sm"
          className="h-7 gap-1.5 px-2 font-mono text-[10px]"
          onClick={() => setNewOpen(true)}
        >
          <PlusIcon className="size-3" />
          New
        </Button>
      </div>

      {isLoading ? (
        <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-28 w-full rounded-md" />
          ))}
        </div>
      ) : totalCount === 0 ? (
        <EmptyState
          title="No schedules"
          description="Create a schedule to run the agent on a cron."
        />
      ) : (
        <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
          {(schedules ?? []).map((schedule) => (
            <ScheduleCard
              key={schedule.id}
              schedule={schedule}
              onToggle={() => {
                toggleSchedule.mutate(
                  { id: schedule.id, enabled: !schedule.enabled },
                  { onError: (err) => toast.error(`Failed: ${err.message}`) },
                )
              }}
              onDelete={() => setDeleteTarget(schedule)}
              togglePending={toggleSchedule.isPending && toggleSchedule.variables?.id === schedule.id}
            />
          ))}
        </div>
      )}

      <NewScheduleDialog open={newOpen} onClose={() => setNewOpen(false)} />
      <ConfirmDeleteDialog
        open={Boolean(deleteTarget)}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget) return
          deleteSchedule.mutate(deleteTarget.id, {
            onSuccess: () => {
              toast.success(`Schedule "${deleteTarget.id}" deleted`)
              setDeleteTarget(null)
            },
            onError: (err) => {
              toast.error(`Failed to delete: ${err.message}`)
            },
          })
        }}
        isPending={deleteSchedule.isPending}
        title="Delete Schedule"
        description={`Delete schedule "${deleteTarget?.id ?? ''}"? This cannot be undone.`}
      />
    </div>
  )
}

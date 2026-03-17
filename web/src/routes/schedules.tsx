import * as React from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { MoreHorizontalIcon, PlusIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useSchedules } from '@/lib/api/queries/schedules'
import {
  useCreateSchedule,
  useDeleteSchedule,
  useToggleSchedule,
} from '@/lib/api/mutations/schedules'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import type { Schedule } from '@/lib/api/types'

export const Route = createFileRoute('/schedules')({
  component: SchedulesPage,
})

function truncate(text: string, max = 80): string {
  if (text.length <= max) return text
  return text.slice(0, max) + '…'
}

interface DeleteDialogProps {
  schedule: Schedule | null
  onClose: () => void
}

function DeleteDialog({ schedule, onClose }: DeleteDialogProps) {
  const deleteSchedule = useDeleteSchedule()

  function handleDelete() {
    if (!schedule) return
    deleteSchedule.mutate(schedule.id, {
      onSuccess: () => {
        toast.success(`Schedule "${schedule.id}" deleted`)
        onClose()
      },
      onError: (err) => {
        toast.error(`Failed to delete: ${err.message}`)
      },
    })
  }

  return (
    <Dialog open={Boolean(schedule)} onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Delete Schedule</DialogTitle>
        </DialogHeader>
        <p className="font-mono text-xs text-muted-foreground">
          Delete schedule <span className="text-foreground">{schedule?.id}</span>? This cannot be undone.
        </p>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={handleDelete}
            disabled={deleteSchedule.isPending}
          >
            {deleteSchedule.isPending ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

interface NewScheduleDialogProps {
  open: boolean
  onClose: () => void
}

function NewScheduleDialog({ open, onClose }: NewScheduleDialogProps) {
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
  const [newOpen, setNewOpen] = React.useState(false)
  const [deleteTarget, setDeleteTarget] = React.useState<Schedule | null>(null)

  function handleToggle(schedule: Schedule) {
    toggleSchedule.mutate(
      { id: schedule.id, enabled: !schedule.enabled },
      {
        onError: (err) => {
          toast.error(`Failed to toggle: ${err.message}`)
        },
      },
    )
  }

  const columns: ColumnDef<Schedule, unknown>[] = React.useMemo(
    () => [
      {
        accessorKey: 'id',
        header: 'ID',
        cell: ({ row }) => (
          <span className="font-mono text-xs font-medium">{row.original.id}</span>
        ),
      },
      {
        accessorKey: 'cron_expression',
        header: 'Cron',
        cell: ({ row }) => (
          <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs">
            {row.original.cron_expression}
          </code>
        ),
      },
      {
        accessorKey: 'destination',
        header: 'Destination',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.destination}
          </span>
        ),
      },
      {
        accessorKey: 'prompt',
        header: 'Prompt',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {truncate(row.original.prompt)}
          </span>
        ),
      },
      {
        accessorKey: 'enabled',
        header: 'Enabled',
        cell: ({ row }) => (
          <Switch
            checked={row.original.enabled}
            onCheckedChange={() => handleToggle(row.original)}
            disabled={toggleSchedule.isPending}
            size="sm"
          />
        ),
      },
      {
        id: 'actions',
        header: '',
        cell: ({ row }) => (
          <div className="flex justify-end">
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon-sm">
                  <MoreHorizontalIcon className="size-4" />
                  <span className="sr-only">Actions</span>
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem
                  variant="destructive"
                  onSelect={() => setDeleteTarget(row.original)}
                >
                  Delete
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        ),
      },
    ],
    [toggleSchedule.isPending],
  )

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Schedules
        </h1>
        <Button
          size="sm"
          className="font-mono text-xs gap-1.5"
          onClick={() => setNewOpen(true)}
        >
          <PlusIcon className="size-3" />
          New Schedule
        </Button>
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 5 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded" />
          ))}
        </div>
      ) : (schedules ?? []).length === 0 ? (
        <EmptyState
          title="No schedules found"
          description="Create a schedule to run the agent on a cron."
        />
      ) : (
        <DataTable columns={columns} data={schedules ?? []} />
      )}

      <NewScheduleDialog open={newOpen} onClose={() => setNewOpen(false)} />
      <DeleteDialog schedule={deleteTarget} onClose={() => setDeleteTarget(null)} />
    </div>
  )
}

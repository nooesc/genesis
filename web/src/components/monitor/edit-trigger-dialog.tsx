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
import { useSchedules } from '@/lib/api/queries/schedules'
import { useToggleSchedule } from '@/lib/api/mutations/schedules'

interface EditTriggerDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  scheduleId: string | null
}

export function EditTriggerDialog({ open, onOpenChange, scheduleId }: EditTriggerDialogProps) {
  const navigate = useNavigate()
  const targetScheduleId = open ? scheduleId ?? '' : ''
  const { data: schedules, isLoading } = useSchedules({ enabled: open })
  const toggleSchedule = useToggleSchedule()
  const schedule = schedules?.find(item => item.id === targetScheduleId) ?? null

  if (!scheduleId) return null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Trigger details</DialogTitle>
          <DialogDescription className="font-mono text-xs">
            Inspect the schedule and toggle its enabled state.
          </DialogDescription>
        </DialogHeader>

        {isLoading ? (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Loading trigger...
          </div>
        ) : schedule ? (
          <div className="space-y-3">
            <div className="space-y-1">
              <h2 className="font-mono text-base font-semibold text-foreground">{schedule.id}</h2>
              <p className="font-mono text-xs text-muted-foreground/70">
                {schedule.cron_expression} · {schedule.destination}
              </p>
            </div>

            <div className="flex flex-wrap gap-2">
              <Badge variant={schedule.enabled ? 'secondary' : 'outline'} className="font-mono text-[10px] uppercase tracking-[0.18em]">
                {schedule.enabled ? 'enabled' : 'disabled'}
              </Badge>
              {schedule.last_run_at && (
                <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
                  last run {schedule.last_run_at}
                </Badge>
              )}
            </div>

            {schedule.prompt && (
              <pre className="max-h-56 overflow-auto rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-[11px] leading-relaxed text-foreground/80">
                {schedule.prompt}
              </pre>
            )}
          </div>
        ) : (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Trigger not found.
          </div>
        )}

        <DialogFooter>
          {schedule && (
            <Button
              type="button"
              variant="outline"
              onClick={() => toggleSchedule.mutate({ id: schedule.id, enabled: !schedule.enabled })}
              disabled={toggleSchedule.isPending}
              className="font-mono text-[11px] uppercase tracking-[0.18em]"
            >
              {schedule.enabled ? 'Disable trigger' : 'Enable trigger'}
            </Button>
          )}
          <Button
            type="button"
            variant="outline"
            onClick={() => navigate({ to: '/schedules' })}
            className="font-mono text-[11px] uppercase tracking-[0.18em]"
          >
            Open schedules
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

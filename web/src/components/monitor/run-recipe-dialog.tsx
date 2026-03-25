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
import { useSkill, useSkillUsage } from '@/lib/api/queries/skills'

interface RunRecipeDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  skillName: string | null
}

export function RunRecipeDialog({ open, onOpenChange, skillName }: RunRecipeDialogProps) {
  const navigate = useNavigate()
  const targetSkillName = open ? skillName ?? '' : ''
  const { data: skill, isLoading: skillLoading } = useSkill(targetSkillName)
  const { data: usage, isLoading: usageLoading } = useSkillUsage(targetSkillName)

  if (!skillName) return null

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Recipe details</DialogTitle>
          <DialogDescription className="font-mono text-xs">
            Review the saved skill before opening the main skills page.
          </DialogDescription>
        </DialogHeader>

        {skillLoading ? (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Loading recipe...
          </div>
        ) : skill ? (
          <div className="space-y-3">
            <div className="space-y-1">
              <h2 className="font-mono text-base font-semibold text-foreground">{skill.name}</h2>
              {skill.description && (
                <p className="font-mono text-xs text-muted-foreground/70">{skill.description}</p>
              )}
            </div>

            {skill.tags.length > 0 && (
              <div className="flex flex-wrap gap-2">
                {skill.tags.map(tag => (
                  <Badge key={tag} variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
                    {tag}
                  </Badge>
                ))}
              </div>
            )}

            <div className="grid grid-cols-2 gap-2 font-mono text-xs">
              <div className="rounded-lg border border-border/20 bg-background/40 p-2">
                <dt className="text-muted-foreground/50">Name</dt>
                <dd className="truncate text-foreground/80">{skill.name}</dd>
              </div>
              <div className="rounded-lg border border-border/20 bg-background/40 p-2">
                <dt className="text-muted-foreground/50">Usage</dt>
                <dd className="text-foreground/80">
                  {usageLoading ? 'loading...' : usage ? `${usage.stats.total_uses} runs` : 'unknown'}
                </dd>
              </div>
            </div>

            {skill.instructions && (
              <pre className="max-h-56 overflow-auto rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-[11px] leading-relaxed text-foreground/80">
                {skill.instructions}
              </pre>
            )}
          </div>
        ) : (
          <div className="rounded-lg border border-border/20 bg-muted/20 p-3 font-mono text-xs text-muted-foreground/70">
            Recipe not found.
          </div>
        )}

        <DialogFooter>
          <Button
            type="button"
            variant="outline"
            onClick={() => navigate({ to: '/skills' })}
            className="font-mono text-[11px] uppercase tracking-[0.18em]"
          >
            Open skills
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

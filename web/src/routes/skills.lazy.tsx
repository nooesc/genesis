import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import { PlusIcon, TagIcon, Trash2Icon, ChevronDown, ChevronRight, Brain } from 'lucide-react'
import { toast } from 'sonner'
import { useSkills } from '@/lib/api/queries/skills'
import { useCreateSkill, useDeleteSkill } from '@/lib/api/mutations/skills'
import { EmptyState } from '@/components/shared/empty-state'
import { ConfirmDeleteDialog } from '@/components/shared/confirm-delete-dialog'
import { Badge } from '@/components/ui/badge'
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
import { Textarea } from '@/components/ui/textarea'
import type { Skill } from '@/lib/api/types'
import { formatRelativeTime } from '@/lib/utils'

export const Route = createLazyFileRoute('/skills')({
  component: SkillsPage,
})

function SkillCard({
  skill,
  onDelete,
}: {
  skill: Skill
  onDelete: () => void
}) {
  const [expanded, setExpanded] = React.useState(false)
  const hasInstructions = skill.instructions && skill.instructions.length > 0

  return (
    <div className="group rounded-md border border-border/30 bg-card/30 p-3 transition-colors hover:border-border/50">
      {/* Header: name + tags + actions */}
      <div className="mb-1.5 flex items-start gap-2">
        <Brain className="mt-0.5 h-3.5 w-3.5 shrink-0 text-muted-foreground/30" />
        <div className="min-w-0 flex-1">
          <div className="font-mono text-[11px] font-medium text-foreground/80">{skill.name}</div>
          {skill.description && (
            <div className="mt-0.5 font-mono text-[10px] leading-relaxed text-muted-foreground/50">
              {skill.description}
            </div>
          )}
        </div>
        <button
          onClick={onDelete}
          className="flex h-5 w-5 shrink-0 items-center justify-center rounded text-muted-foreground/30 opacity-0 transition-all hover:text-destructive group-hover:opacity-100 group-focus-within:opacity-100"
          title="Delete"
        >
          <Trash2Icon className="h-3 w-3" />
        </button>
      </div>

      {/* Tags */}
      {skill.tags.length > 0 && (
        <div className="mb-1.5 flex flex-wrap gap-1">
          {skill.tags.map((tag) => (
            <Badge key={tag} variant="outline" className="font-mono text-[8px] uppercase">
              {tag}
            </Badge>
          ))}
        </div>
      )}

      {/* Expandable content */}
      {hasInstructions && (
        <button
          onClick={() => setExpanded(v => !v)}
          className="flex items-center gap-1 font-mono text-[9px] text-muted-foreground/40 transition-colors hover:text-muted-foreground"
        >
          {expanded ? <ChevronDown className="h-3 w-3" /> : <ChevronRight className="h-3 w-3" />}
          {expanded ? 'hide instructions' : 'show instructions'}
        </button>
      )}
      {expanded && hasInstructions && (
        <pre className="mt-1.5 max-h-48 overflow-auto whitespace-pre-wrap rounded bg-muted/20 px-2 py-1.5 font-mono text-[10px] leading-relaxed text-foreground/50">
          {skill.instructions}
        </pre>
      )}

      {/* Footer: timestamp */}
      <div className="mt-2 font-mono text-[8px] text-muted-foreground/30">
        Updated {formatRelativeTime(skill.updated_at)}
      </div>
    </div>
  )
}

function NewSkillDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const createSkill = useCreateSkill()
  const [name, setName] = React.useState('')
  const [description, setDescription] = React.useState('')
  const [instructions, setInstructions] = React.useState('')
  const [tags, setTags] = React.useState('')

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim()) return
    createSkill.mutate(
        {
          name: name.trim(),
          description: description.trim(),
          instructions: instructions.trim(),
          tags: tags.split(',').map(t => t.trim()).filter(Boolean),
        },
      {
        onSuccess: () => {
          toast.success(`Skill "${name}" created`)
          handleClose()
        },
        onError: (err) => {
          toast.error(`Failed to create: ${err.message}`)
        },
      },
    )
  }

  function handleClose() {
    setName('')
    setDescription('')
    setInstructions('')
    setTags('')
    onClose()
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && handleClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">New Skill</DialogTitle>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Name</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="skill-name" className="font-mono text-xs" required />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Description</Label>
            <Input value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Brief description" className="font-mono text-xs" />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Instructions</Label>
            <Textarea value={instructions} onChange={(e) => setInstructions(e.target.value)} placeholder="Skill instructions..." className="font-mono text-xs" rows={5} />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Tags (comma-separated)</Label>
            <Input value={tags} onChange={(e) => setTags(e.target.value)} placeholder="tag1, tag2" className="font-mono text-xs" />
          </div>
          <DialogFooter className="-mx-0 -mb-0 border-0 bg-transparent p-0 pt-1">
            <Button type="button" variant="outline" size="sm" onClick={handleClose}>Cancel</Button>
            <Button type="submit" size="sm" disabled={createSkill.isPending || !name.trim()}>
              {createSkill.isPending ? 'Creating...' : 'Create'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function SkillsPage() {
  const { data: skills, isLoading } = useSkills()
  const deleteSkill = useDeleteSkill()
  const [search, setSearch] = React.useState('')
  const [tagFilter, setTagFilter] = React.useState<string | null>(null)
  const [newOpen, setNewOpen] = React.useState(false)
  const [deleteTarget, setDeleteTarget] = React.useState<Skill | null>(null)

  const allTags = React.useMemo(() => {
    const tagSet = new Set<string>()
    for (const skill of skills ?? []) {
      for (const tag of skill.tags) tagSet.add(tag)
    }
    return Array.from(tagSet).sort()
  }, [skills])

  const filtered = React.useMemo(() => {
    return (skills ?? []).filter((skill) => {
      const matchesSearch = !search ||
        skill.name.toLowerCase().includes(search.toLowerCase()) ||
        skill.description.toLowerCase().includes(search.toLowerCase())
      const matchesTag = !tagFilter || skill.tags.includes(tagFilter)
      return matchesSearch && matchesTag
    })
  }, [skills, search, tagFilter])

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
            Skills
          </h1>
          {!isLoading && (
            <span className="font-mono text-[10px] tabular-nums text-muted-foreground/30">
              {filtered.length}
            </span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 font-mono text-[11px]"
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm" className="h-7 gap-1.5 px-2 font-mono text-[10px]">
                <TagIcon className="size-3" />
                {tagFilter ?? 'All'}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => setTagFilter(null)}>
                <span className="font-mono text-xs">All tags</span>
              </DropdownMenuItem>
              {allTags.map((tag) => (
                <DropdownMenuItem key={tag} onSelect={() => setTagFilter(tag)}>
                  <span className="font-mono text-xs">{tag}</span>
                </DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
          <Button size="sm" className="h-7 gap-1.5 px-2 font-mono text-[10px]" onClick={() => setNewOpen(true)}>
            <PlusIcon className="size-3" />
            New
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-24 w-full rounded-md" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          title="No skills found"
          description={search || tagFilter ? 'Try adjusting your filters.' : 'Create your first skill.'}
        />
      ) : (
        <div className="card-stagger grid grid-cols-1 gap-2 lg:grid-cols-2">
          {filtered.map((skill) => (
            <SkillCard
              key={skill.name}
              skill={skill}
              onDelete={() => setDeleteTarget(skill)}
            />
          ))}
        </div>
      )}

      <NewSkillDialog open={newOpen} onClose={() => setNewOpen(false)} />
      <ConfirmDeleteDialog
        open={Boolean(deleteTarget)}
        onClose={() => setDeleteTarget(null)}
        onConfirm={() => {
          if (!deleteTarget) return
          deleteSkill.mutate(deleteTarget.name, {
            onSuccess: () => {
              toast.success(`Skill "${deleteTarget.name}" deleted`)
              setDeleteTarget(null)
            },
            onError: (err) => {
              toast.error(`Failed to delete: ${err.message}`)
            },
          })
        }}
        isPending={deleteSkill.isPending}
        title="Delete Skill"
        description={`Delete "${deleteTarget?.name ?? ''}"? This cannot be undone.`}
      />
    </div>
  )
}

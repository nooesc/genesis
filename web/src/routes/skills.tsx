import * as React from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { type ColumnDef } from '@tanstack/react-table'
import { MoreHorizontalIcon, PlusIcon, TagIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useSkills } from '@/lib/api/queries/skills'
import { useCreateSkill, useDeleteSkill } from '@/lib/api/mutations/skills'
import { DataTable } from '@/components/shared/data-table'
import { EmptyState } from '@/components/shared/empty-state'
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

export const Route = createFileRoute('/skills')({
  component: SkillsPage,
})

function formatRelativeTime(isoString: string): string {
  const now = Date.now()
  const then = new Date(isoString).getTime()
  const diffMs = now - then
  const diffSec = Math.floor(diffMs / 1000)
  if (diffSec < 60) return `${diffSec}s ago`
  const diffMin = Math.floor(diffSec / 60)
  if (diffMin < 60) return `${diffMin}m ago`
  const diffHr = Math.floor(diffMin / 60)
  if (diffHr < 24) return `${diffHr}h ago`
  const diffDay = Math.floor(diffHr / 24)
  return `${diffDay}d ago`
}

interface DeleteDialogProps {
  skill: Skill | null
  onClose: () => void
}

function DeleteDialog({ skill, onClose }: DeleteDialogProps) {
  const deleteSkill = useDeleteSkill()

  function handleDelete() {
    if (!skill) return
    deleteSkill.mutate(skill.name, {
      onSuccess: () => {
        toast.success(`Skill "${skill.name}" deleted`)
        onClose()
      },
      onError: (err) => {
        toast.error(`Failed to delete: ${err.message}`)
      },
    })
  }

  return (
    <Dialog open={Boolean(skill)} onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle className="font-mono text-sm">Delete Skill</DialogTitle>
        </DialogHeader>
        <p className="font-mono text-xs text-muted-foreground">
          Delete <span className="text-foreground">{skill?.name}</span>? This cannot be undone.
        </p>
        <DialogFooter>
          <Button variant="outline" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="destructive"
            size="sm"
            onClick={handleDelete}
            disabled={deleteSkill.isPending}
          >
            {deleteSkill.isPending ? 'Deleting...' : 'Delete'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

interface NewSkillDialogProps {
  open: boolean
  onClose: () => void
}

function NewSkillDialog({ open, onClose }: NewSkillDialogProps) {
  const createSkill = useCreateSkill()
  const [name, setName] = React.useState('')
  const [description, setDescription] = React.useState('')
  const [content, setContent] = React.useState('')
  const [tags, setTags] = React.useState('')

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim()) return
    createSkill.mutate(
      {
        name: name.trim(),
        description: description.trim(),
        content: content.trim(),
        tags: tags
          .split(',')
          .map((t) => t.trim())
          .filter(Boolean),
      },
      {
        onSuccess: () => {
          toast.success(`Skill "${name}" created`)
          setName('')
          setDescription('')
          setContent('')
          setTags('')
          onClose()
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
    setContent('')
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
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="skill-name"
              className="font-mono text-xs"
              required
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Description</Label>
            <Input
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Brief description"
              className="font-mono text-xs"
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Content</Label>
            <Textarea
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder="Skill content / instructions..."
              className="font-mono text-xs"
              rows={5}
            />
          </div>
          <div className="flex flex-col gap-1.5">
            <Label className="font-mono text-xs">Tags (comma-separated)</Label>
            <Input
              value={tags}
              onChange={(e) => setTags(e.target.value)}
              placeholder="tag1, tag2, tag3"
              className="font-mono text-xs"
            />
          </div>
          <DialogFooter className="-mx-0 -mb-0 border-0 bg-transparent p-0 pt-1">
            <Button type="button" variant="outline" size="sm" onClick={handleClose}>
              Cancel
            </Button>
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
  const [search, setSearch] = React.useState('')
  const [tagFilter, setTagFilter] = React.useState<string | null>(null)
  const [newOpen, setNewOpen] = React.useState(false)
  const [deleteTarget, setDeleteTarget] = React.useState<Skill | null>(null)

  const allTags = React.useMemo(() => {
    const tagSet = new Set<string>()
    for (const skill of skills ?? []) {
      for (const tag of skill.tags) {
        tagSet.add(tag)
      }
    }
    return Array.from(tagSet).sort()
  }, [skills])

  const filtered = React.useMemo(() => {
    return (skills ?? []).filter((skill) => {
      const matchesSearch =
        !search ||
        skill.name.toLowerCase().includes(search.toLowerCase()) ||
        skill.description.toLowerCase().includes(search.toLowerCase())
      const matchesTag = !tagFilter || skill.tags.includes(tagFilter)
      return matchesSearch && matchesTag
    })
  }, [skills, search, tagFilter])

  const columns: ColumnDef<Skill, unknown>[] = React.useMemo(
    () => [
      {
        accessorKey: 'name',
        header: 'Name',
        cell: ({ row }) => (
          <span className="font-mono text-xs font-medium">{row.original.name}</span>
        ),
      },
      {
        accessorKey: 'tags',
        header: 'Tags',
        cell: ({ row }) => (
          <div className="flex flex-wrap gap-1">
            {row.original.tags.length > 0 ? (
              row.original.tags.map((tag) => (
                <Badge key={tag} variant="outline" className="font-mono text-[10px]">
                  {tag}
                </Badge>
              ))
            ) : (
              <span className="text-muted-foreground">—</span>
            )}
          </div>
        ),
      },
      {
        id: 'updated_at',
        header: 'Last Updated',
        accessorKey: 'updated_at',
        cell: ({ row }) => (
          <span className="font-mono text-xs text-muted-foreground">
            {formatRelativeTime(row.original.updated_at)}
          </span>
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
    [],
  )

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
          Skills
        </h1>
        <div className="flex items-center gap-2">
          <Input
            placeholder="Search skills..."
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            className="w-48 font-mono text-xs"
          />
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="outline" size="sm" className="font-mono text-xs gap-1.5">
                <TagIcon className="size-3" />
                {tagFilter ?? 'All tags'}
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
          <Button size="sm" className="font-mono text-xs gap-1.5" onClick={() => setNewOpen(true)}>
            <PlusIcon className="size-3" />
            New Skill
          </Button>
        </div>
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-2">
          {Array.from({ length: 6 }).map((_, i) => (
            <Skeleton key={i} className="h-10 w-full rounded" />
          ))}
        </div>
      ) : filtered.length === 0 ? (
        <EmptyState
          title="No skills found"
          description={search || tagFilter ? 'Try adjusting your filters.' : 'Create your first skill to get started.'}
        />
      ) : (
        <DataTable columns={columns} data={filtered} />
      )}

      <NewSkillDialog open={newOpen} onClose={() => setNewOpen(false)} />
      <DeleteDialog skill={deleteTarget} onClose={() => setDeleteTarget(null)} />
    </div>
  )
}

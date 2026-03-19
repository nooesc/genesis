import * as React from 'react'
import { createLazyFileRoute } from '@tanstack/react-router'
import {
  EyeIcon, EyeOffIcon, CheckIcon, Trash2Icon,
  ShieldCheckIcon, ShieldAlertIcon, XIcon,
} from 'lucide-react'
import { toast } from 'sonner'
import { useMcpStatus } from '@/lib/api/queries/health'
import { useApprovedUsers, usePendingPairings } from '@/lib/api/queries/pairing'
import { useApprovePairing, useRevokePairing, useClearPending } from '@/lib/api/mutations/pairing'
import { setApiKey, clearApiKey, hasApiKey } from '@/lib/api/client'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import { formatRelativeTime } from '@/lib/utils'
import { getPlatformColor } from '@/lib/platforms'
import { SectionHeader } from '@/components/shared/section-header'
import { PageHeader } from '@/components/shared/page-header'
import { Settings as SettingsIcon } from 'lucide-react'
import type { ApprovedUser, PendingPairing } from '@/lib/api/types'

export const Route = createLazyFileRoute('/settings')({
  component: SettingsPage,
})

// SectionHeader imported from shared component

// --- API Key section ---

function ApiKeySection() {
  const [keyInput, setKeyInput] = React.useState('')
  const [showKey, setShowKey] = React.useState(false)
  const [saved, setSaved] = React.useState(false)
  const [hasKey, setHasKey] = React.useState(hasApiKey())

  function handleSave(e: React.FormEvent) {
    e.preventDefault()
    if (!keyInput.trim()) return
    setApiKey(keyInput.trim())
    setHasKey(true)
    setKeyInput('')
    setSaved(true)
    setTimeout(() => setSaved(false), 2000)
  }

  function handleClear() {
    clearApiKey()
    setHasKey(false)
    setKeyInput('')
  }

  return (
    <div className="rounded-md border border-border/20 bg-card/20 p-3">
      <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
        API Key
      </div>
      <p className="mb-3 font-mono text-[10px] text-muted-foreground/40">
        Bearer token for authenticated gateway requests. Stored in{' '}
        <code className="text-foreground/50">localStorage</code>.
      </p>

      {hasKey && (
        <div className="mb-3 flex items-center gap-2 rounded border border-border/15 bg-muted/10 px-2.5 py-1.5">
          <CheckIcon className="size-3 text-emerald-400" />
          <span className="font-mono text-[10px] text-muted-foreground/50">Key configured</span>
          <button
            onClick={handleClear}
            className="ml-auto flex items-center gap-1 font-mono text-[9px] text-destructive/60 transition-colors hover:text-destructive"
          >
            <Trash2Icon className="size-2.5" />
            Clear
          </button>
        </div>
      )}

      <form onSubmit={handleSave} className="flex items-center gap-2">
        <div className="relative flex-1">
          <Input
            type={showKey ? 'text' : 'password'}
            value={keyInput}
            onChange={(e) => setKeyInput(e.target.value)}
            placeholder={hasKey ? 'Enter new key to replace…' : 'Paste your API key…'}
            className="pr-8 font-mono text-[10px]"
          />
          <button
            type="button"
            onClick={() => setShowKey((v) => !v)}
            className="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground/30 hover:text-muted-foreground"
            aria-label={showKey ? 'Hide key' : 'Show key'}
          >
            {showKey ? <EyeOffIcon className="size-3" /> : <EyeIcon className="size-3" />}
          </button>
        </div>
        <Button type="submit" size="sm" className="h-7 px-3 font-mono text-[10px]" disabled={!keyInput.trim()}>
          {saved ? 'Saved!' : 'Save'}
        </Button>
      </form>
    </div>
  )
}

// --- MCP Servers section ---

function McpServersSection() {
  const { data: mcpStatus, isLoading } = useMcpStatus()

  return (
    <div className="rounded-md border border-border/20 bg-card/20 p-3">
      <div className="mb-2 font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
        MCP Servers
      </div>
      {isLoading ? (
        <div className="flex flex-col gap-1.5">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-7 w-full rounded" />
          ))}
        </div>
      ) : !mcpStatus || mcpStatus.servers.length === 0 ? (
        <p className="font-mono text-[10px] text-muted-foreground/40">No MCP servers configured.</p>
      ) : (
        <div className="flex flex-col gap-1.5">
          {mcpStatus.servers.map((server) => (
            <div
              key={server.name}
              className="flex items-center justify-between rounded border border-border/10 px-2.5 py-1.5"
            >
              <span className="font-mono text-[10px] font-medium text-foreground/70">
                {server.name}
              </span>
              <div className="flex items-center gap-1.5">
                <span
                  className={`h-1.5 w-1.5 rounded-full ${
                    server.connected ? 'bg-emerald-400' : 'bg-red-400'
                  }`}
                />
                <span className="font-mono text-[8px] uppercase tracking-wider text-muted-foreground/30">
                  {server.connected ? 'connected' : 'disconnected'}
                </span>
              </div>
            </div>
          ))}
          <div className="mt-1 flex items-center gap-3 font-mono text-[8px] text-muted-foreground/25">
            <span>{mcpStatus.total_tools} tools</span>
            <span>{mcpStatus.total_resources} resources</span>
            <span>{mcpStatus.total_prompts} prompts</span>
          </div>
        </div>
      )}
    </div>
  )
}

// --- Pairing section ---

function ApprovedUserRow({
  user,
  onRevoke,
  isPending,
}: {
  user: ApprovedUser
  onRevoke: () => void
  isPending: boolean
}) {
  const color = getPlatformColor(user.platform)

  return (
    <div className="group flex items-center gap-2 rounded border border-border/10 px-2.5 py-1.5">
      <ShieldCheckIcon className="h-3 w-3 shrink-0 text-emerald-400/60" />
      <span
        className="font-mono text-[8px] uppercase tracking-wider"
        style={{ color }}
      >
        {user.platform}
      </span>
      <span className="font-mono text-[10px] text-foreground/70">
        {user.user_name || user.user_id}
      </span>
      <span className="ml-auto font-mono text-[8px] tabular-nums text-muted-foreground/25">
        {formatRelativeTime(user.approved_at)}
      </span>
      <button
        onClick={onRevoke}
        disabled={isPending}
        className="flex h-4 w-4 items-center justify-center rounded text-muted-foreground/20 opacity-0 transition-all hover:text-destructive group-hover:opacity-100"
        title="Revoke"
      >
        <XIcon className="h-2.5 w-2.5" />
      </button>
    </div>
  )
}

function PendingPairingRow({
  pairing,
  onApprove,
  isPending,
}: {
  pairing: PendingPairing
  onApprove: () => void
  isPending: boolean
}) {
  const color = getPlatformColor(pairing.platform)

  return (
    <div className="flex items-center gap-2 rounded border border-amber-400/10 bg-amber-400/5 px-2.5 py-1.5">
      <ShieldAlertIcon className="h-3 w-3 shrink-0 text-amber-400/60" />
      <span
        className="font-mono text-[8px] uppercase tracking-wider"
        style={{ color }}
      >
        {pairing.platform}
      </span>
      <span className="font-mono text-[10px] text-foreground/70">
        {pairing.user_name || pairing.user_id}
      </span>
      <Badge variant="outline" className="font-mono text-[8px] tabular-nums">
        {pairing.code}
      </Badge>
      <span className="ml-auto font-mono text-[8px] tabular-nums text-muted-foreground/25">
        {formatRelativeTime(pairing.created_at)}
      </span>
      <Button
        size="sm"
        className="h-5 px-2 font-mono text-[8px]"
        onClick={onApprove}
        disabled={isPending}
      >
        Approve
      </Button>
    </div>
  )
}

function PairingSection() {
  const { data: approvedData, isLoading: loadingApproved } = useApprovedUsers()
  const { data: pendingData, isLoading: loadingPending } = usePendingPairings()
  const approveMut = useApprovePairing()
  const revokeMut = useRevokePairing()
  const clearMut = useClearPending()

  const approved = approvedData?.approved ?? []
  const pending = pendingData?.pending ?? []
  const isLoading = loadingApproved || loadingPending

  return (
    <div className="rounded-md border border-border/20 bg-card/20 p-3">
      <div className="mb-2 flex items-center justify-between">
        <div className="font-mono text-[9px] uppercase tracking-wider text-muted-foreground/40">
          Platform Pairing
        </div>
        {pending.length > 0 && (
          <button
            onClick={() => {
              clearMut.mutate(undefined, {
                onSuccess: () => toast.success('Pending codes cleared'),
                onError: (err: Error) => toast.error(`Failed: ${err.message}`),
              })
            }}
            disabled={clearMut.isPending}
            className="font-mono text-[8px] text-muted-foreground/30 transition-colors hover:text-muted-foreground"
          >
            Clear pending
          </button>
        )}
      </div>

      {isLoading ? (
        <div className="flex flex-col gap-1.5">
          {Array.from({ length: 3 }).map((_, i) => (
            <Skeleton key={i} className="h-7 w-full rounded" />
          ))}
        </div>
      ) : approved.length === 0 && pending.length === 0 ? (
        <p className="font-mono text-[10px] text-muted-foreground/40">
          No paired users. Send a message on a platform to start pairing.
        </p>
      ) : (
        <div className="flex flex-col gap-3">
          {/* Pending approvals */}
          {pending.length > 0 && (
            <div className="flex flex-col gap-1.5">
              <span className="font-mono text-[8px] uppercase tracking-widest text-amber-400/50">
                Pending ({pending.length})
              </span>
              {pending.map((p) => (
                <PendingPairingRow
                  key={`${p.platform}:${p.code}`}
                  pairing={p}
                  onApprove={() => {
                    approveMut.mutate(
                      { platform: p.platform, code: p.code },
                      {
                        onSuccess: () => toast.success(`Approved ${p.user_name || p.user_id}`),
                        onError: (err: Error) => toast.error(`Failed: ${err.message}`),
                      },
                    )
                  }}
                  isPending={approveMut.isPending}
                />
              ))}
            </div>
          )}

          {/* Approved users */}
          {approved.length > 0 && (
            <div className="flex flex-col gap-1.5">
              <span className="font-mono text-[8px] uppercase tracking-widest text-emerald-400/50">
                Approved ({approved.length})
              </span>
              {approved.map((u) => (
                <ApprovedUserRow
                  key={`${u.platform}:${u.user_id}`}
                  user={u}
                  onRevoke={() => {
                    revokeMut.mutate(
                      { platform: u.platform, user_id: u.user_id },
                      {
                        onSuccess: () => toast.success(`Revoked ${u.user_name || u.user_id}`),
                        onError: (err: Error) => toast.error(`Failed: ${err.message}`),
                      },
                    )
                  }}
                  isPending={revokeMut.isPending}
                />
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

// --- Settings page ---

function SettingsPage() {
  return (
    <div className="flex flex-col gap-5">
      <PageHeader title="Settings" icon={SettingsIcon} />

      <SectionHeader title="Authentication" />
      <ApiKeySection />

      <SectionHeader title="Infrastructure" />
      <McpServersSection />

      <SectionHeader title="Access Control" />
      <PairingSection />
    </div>
  )
}

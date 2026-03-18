import * as React from 'react'
import { createFileRoute } from '@tanstack/react-router'
import { EyeIcon, EyeOffIcon, CheckIcon, Trash2Icon } from 'lucide-react'
import { useMcpStatus } from '@/lib/api/queries/health'
import { setApiKey, clearApiKey, hasApiKey } from '@/lib/api/client'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'

export const Route = createFileRoute('/settings')({
  component: SettingsPage,
})

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
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
          API Key
        </CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4 pb-4">
        <p className="font-mono text-xs text-muted-foreground">
          Set a Bearer token for authenticated requests to the Genesis gateway.
          Stored in <code className="text-foreground">localStorage</code>.
        </p>

        {hasKey && (
          <div className="flex items-center gap-2 rounded-md border border-border bg-muted px-3 py-2">
            <CheckIcon className="size-3 text-green-500" />
            <span className="font-mono text-xs text-muted-foreground">API key is configured</span>
            <Button
              variant="ghost"
              size="xs"
              className="ml-auto font-mono text-xs text-destructive hover:text-destructive"
              onClick={handleClear}
            >
              <Trash2Icon className="size-3" />
              Clear
            </Button>
          </div>
        )}

        <form onSubmit={handleSave} className="flex items-center gap-2">
          <div className="relative flex-1">
            <Input
              type={showKey ? 'text' : 'password'}
              value={keyInput}
              onChange={(e) => setKeyInput(e.target.value)}
              placeholder={hasKey ? 'Enter new key to replace…' : 'Paste your API key…'}
              className="font-mono text-xs pr-10"
            />
            <button
              type="button"
              onClick={() => setShowKey((v) => !v)}
              className="absolute right-2.5 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground"
              aria-label={showKey ? 'Hide key' : 'Show key'}
            >
              {showKey ? <EyeOffIcon className="size-3.5" /> : <EyeIcon className="size-3.5" />}
            </button>
          </div>
          <Button
            type="submit"
            size="sm"
            className="font-mono text-xs"
            disabled={!keyInput.trim()}
          >
            {saved ? 'Saved!' : 'Save'}
          </Button>
        </form>
      </CardContent>
    </Card>
  )
}

// --- MCP Servers section ---

function McpServersSection() {
  const { data: mcpStatus, isLoading } = useMcpStatus()

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
          MCP Servers
        </CardTitle>
      </CardHeader>
      <CardContent className="pb-4">
        {isLoading ? (
          <div className="flex flex-col gap-2">
            {Array.from({ length: 3 }).map((_, i) => (
              <Skeleton key={i} className="h-8 w-full rounded" />
            ))}
          </div>
        ) : !mcpStatus || mcpStatus.servers.length === 0 ? (
          <p className="font-mono text-xs text-muted-foreground">No MCP servers configured.</p>
        ) : (
          <div className="rounded-md border border-border">
            <Table>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
                    Name
                  </TableHead>
                  <TableHead className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
                    Status
                  </TableHead>
                  <TableHead className="font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
                    Tools
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {mcpStatus.servers.map((server) => (
                  <TableRow key={server.name} className="hover:bg-accent">
                    <TableCell className="font-mono text-xs font-medium">
                      {server.name}
                    </TableCell>
                    <TableCell>
                      <Badge
                        variant={server.connected ? 'secondary' : 'destructive'}
                        className="font-mono text-[10px]"
                      >
                        {server.connected ? 'connected' : 'disconnected'}
                      </Badge>
                    </TableCell>
                    <TableCell className="font-mono text-xs text-muted-foreground">
                      {server.tool_count}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// --- Pairing section (placeholder — no hooks yet) ---

function PairingSection() {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="font-mono text-xs uppercase tracking-wider text-muted-foreground">
          Platform Pairing
        </CardTitle>
      </CardHeader>
      <CardContent className="pb-4">
        <p className="font-mono text-xs text-muted-foreground">
          Pairing management is available via the gateway API at{' '}
          <code className="text-foreground">/api/pairing/approved</code> and{' '}
          <code className="text-foreground">/api/pairing/pending</code>.
          UI controls coming soon.
        </p>
      </CardContent>
    </Card>
  )
}

// --- Settings page ---

function SettingsPage() {
  return (
    <div className="flex flex-col gap-6">
      <h1 className="font-mono text-sm font-medium uppercase tracking-wider text-muted-foreground">
        Settings
      </h1>

      <ApiKeySection />
      <McpServersSection />
      <PairingSection />
    </div>
  )
}

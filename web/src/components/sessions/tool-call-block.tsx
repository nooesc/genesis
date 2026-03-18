import * as React from 'react'
import { ChevronDownIcon, ChevronRightIcon, WrenchIcon } from 'lucide-react'
import { cn, truncate } from '@/lib/utils'
import { Badge } from '@/components/ui/badge'

interface ToolCall {
  id?: string
  type?: string
  function?: {
    name?: string
    arguments?: string
  }
}

interface ToolCallBlockProps {
  toolCallsJson: string | null
  result?: string | null
  durationMs?: number | null
  isSuccess?: boolean
}

function parseToolCalls(json: string | null): ToolCall[] {
  if (!json) return []
  try {
    const parsed = JSON.parse(json)
    if (Array.isArray(parsed)) return parsed
    if (typeof parsed === 'object' && parsed !== null) return [parsed]
    return []
  } catch {
    return []
  }
}

function formatJson(s: string): string {
  try {
    return JSON.stringify(JSON.parse(s), null, 2)
  } catch {
    return s
  }
}

export function ToolCallBlock({ toolCallsJson, result, durationMs, isSuccess = true }: ToolCallBlockProps) {
  const [open, setOpen] = React.useState(false)
  const toolCalls = parseToolCalls(toolCallsJson)

  if (toolCalls.length === 0 && !result) return null

  const primaryTool = toolCalls[0]
  const toolName = primaryTool?.function?.name ?? 'tool_call'

  return (
    <div className="rounded-md border border-border bg-muted/20">
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left transition-colors hover:bg-muted/40"
      >
        <WrenchIcon className="size-3 text-muted-foreground" />
        <span className="font-mono text-xs font-medium">{toolName}</span>
        {toolCalls.length > 1 && (
          <span className="font-mono text-[10px] text-muted-foreground">
            +{toolCalls.length - 1} more
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          {durationMs != null && (
            <Badge
              variant="outline"
              className={cn(
                'font-mono text-[10px]',
                isSuccess ? 'border-green-500/30 text-green-400' : 'border-destructive/30 text-destructive'
              )}
            >
              {durationMs < 1000 ? `${durationMs}ms` : `${(durationMs / 1000).toFixed(1)}s`}
            </Badge>
          )}
          {open ? (
            <ChevronDownIcon className="size-3 text-muted-foreground" />
          ) : (
            <ChevronRightIcon className="size-3 text-muted-foreground" />
          )}
        </div>
      </button>

      {open && (
        <div className="border-t border-border px-3 py-2">
          {toolCalls.map((tc, i) => {
            const name = tc.function?.name ?? 'unknown'
            const args = tc.function?.arguments ?? ''
            return (
              <div key={tc.id ?? i} className={cn(i > 0 && 'mt-3 border-t border-border/50 pt-3')}>
                {toolCalls.length > 1 && (
                  <p className="mb-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground">
                    {name}
                  </p>
                )}
                {args && (
                  <div className="mb-2">
                    <p className="mb-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground/60">
                      Params
                    </p>
                    <pre className="overflow-x-auto whitespace-pre-wrap break-all font-mono text-[10px] text-foreground/70">
                      {truncate(formatJson(args), 300)}
                    </pre>
                  </div>
                )}
              </div>
            )
          })}

          {result && (
            <div className={cn(toolCalls.length > 0 && 'mt-2 border-t border-border/50 pt-2')}>
              <p className="mb-1 font-mono text-[10px] uppercase tracking-wider text-muted-foreground/60">
                Result
              </p>
              <pre className="overflow-x-auto whitespace-pre-wrap break-all font-mono text-[10px] text-foreground/70">
                {truncate(result, 300)}
              </pre>
            </div>
          )}
        </div>
      )}
    </div>
  )
}

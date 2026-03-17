import { cn } from '@/lib/utils'
import type { StoredMessage } from '@/lib/api/types'
import { ToolCallBlock } from './tool-call-block'

function formatTime(isoString: string): string {
  const d = new Date(isoString)
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

function formatDate(isoString: string): string {
  const d = new Date(isoString)
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })
}

function roleMeta(role: StoredMessage['role']): { label: string; borderClass: string; labelClass: string } {
  switch (role) {
    case 'user':
      return {
        label: 'user',
        borderClass: 'border-l-2 border-primary',
        labelClass: 'text-primary',
      }
    case 'assistant':
      return {
        label: 'assistant',
        borderClass: 'border-l-2 border-[#a855f7]',
        labelClass: 'text-[#a855f7]',
      }
    case 'tool':
      return {
        label: 'tool',
        borderClass: 'border-l-2 border-muted-foreground/40',
        labelClass: 'text-muted-foreground',
      }
    case 'system':
      return {
        label: 'system',
        borderClass: 'border-l-2 border-yellow-500/40',
        labelClass: 'text-yellow-500/70',
      }
  }
}

interface MessageThreadProps {
  messages: StoredMessage[]
}

export function MessageThread({ messages }: MessageThreadProps) {
  if (messages.length === 0) {
    return (
      <div className="flex items-center justify-center py-12">
        <p className="font-mono text-xs text-muted-foreground">No messages in this session.</p>
      </div>
    )
  }

  // Group messages by date for visual separation
  let lastDate = ''

  return (
    <div className="flex flex-col gap-3">
      {messages.map((msg) => {
        const meta = roleMeta(msg.role)
        const dateStr = formatDate(msg.created_at)
        const showDateSeparator = dateStr !== lastDate
        lastDate = dateStr

        if (msg.role === 'system') {
          return (
            <div key={msg.id}>
              {showDateSeparator && <DateSeparator label={dateStr} />}
              <div className="flex items-start gap-2 py-1">
                <span className={cn('font-mono text-[10px] uppercase tracking-wider', meta.labelClass)}>
                  system
                </span>
                <p className="font-mono text-[10px] text-muted-foreground/60 line-clamp-2">
                  {msg.content}
                </p>
              </div>
            </div>
          )
        }

        if (msg.role === 'tool') {
          return (
            <div key={msg.id}>
              {showDateSeparator && <DateSeparator label={dateStr} />}
              <div className={cn('rounded-r-md pl-3', meta.borderClass)}>
                <div className="mb-1 flex items-center gap-2">
                  <span className={cn('font-mono text-[10px] uppercase tracking-wider', meta.labelClass)}>
                    tool result
                  </span>
                  <span className="font-mono text-[10px] text-muted-foreground/40">
                    {formatTime(msg.created_at)}
                  </span>
                </div>
                <ToolCallBlock
                  toolCallsJson={null}
                  result={msg.content}
                  durationMs={null}
                  isSuccess={true}
                />
              </div>
            </div>
          )
        }

        // user or assistant
        const hasToolCalls = msg.tool_calls_json != null && msg.tool_calls_json !== '[]'

        return (
          <div key={msg.id}>
            {showDateSeparator && <DateSeparator label={dateStr} />}
            <div className={cn('rounded-r-md py-2 pl-3 pr-2', meta.borderClass)}>
              <div className="mb-1 flex items-center gap-2">
                <span className={cn('font-mono text-[10px] uppercase tracking-wider', meta.labelClass)}>
                  {meta.label}
                </span>
                <span className="font-mono text-[10px] text-muted-foreground/40">
                  {formatTime(msg.created_at)}
                </span>
              </div>

              {msg.content && (
                <p className="whitespace-pre-wrap font-mono text-xs leading-relaxed text-foreground/80">
                  {msg.content}
                </p>
              )}

              {hasToolCalls && (
                <div className={cn('mt-2', msg.content && 'mt-3')}>
                  <ToolCallBlock
                    toolCallsJson={msg.tool_calls_json}
                    result={null}
                    durationMs={null}
                    isSuccess={true}
                  />
                </div>
              )}
            </div>
          </div>
        )
      })}
    </div>
  )
}

function DateSeparator({ label }: { label: string }) {
  return (
    <div className="my-4 flex items-center gap-3">
      <div className="h-px flex-1 bg-border" />
      <span className="font-mono text-[10px] text-muted-foreground/40">{label}</span>
      <div className="h-px flex-1 bg-border" />
    </div>
  )
}

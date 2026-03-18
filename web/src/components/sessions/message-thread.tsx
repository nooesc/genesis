import { cn } from '@/lib/utils'
import type { StoredMessage } from '@/lib/api/types'
import { ToolCallBlock } from './tool-call-block'
import { User, Bot, Wrench, Terminal } from 'lucide-react'

function formatTime(isoString: string): string {
  const d = new Date(isoString)
  return d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}

function formatDate(isoString: string): string {
  const d = new Date(isoString)
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })
}

const ROLE_CONFIG = {
  user: {
    label: 'USER',
    color: '#0891b2',
    icon: User,
    nodeClass: 'bg-[#0891b2] ring-[#0891b2]/20',
    lineClass: 'text-[#0891b2]',
  },
  assistant: {
    label: 'EVE',
    color: '#a855f7',
    icon: Bot,
    nodeClass: 'bg-[#a855f7] ring-[#a855f7]/20',
    lineClass: 'text-[#a855f7]',
  },
  tool: {
    label: 'TOOL',
    color: '#525252',
    icon: Wrench,
    nodeClass: 'bg-[#525252] ring-[#525252]/20',
    lineClass: 'text-muted-foreground',
  },
  system: {
    label: 'SYS',
    color: '#eab308',
    icon: Terminal,
    nodeClass: 'bg-[#eab308]/60 ring-[#eab308]/10',
    lineClass: 'text-[#eab308]/60',
  },
} as const

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

  let lastDate = ''

  return (
    <div className="relative">
      {/* Timeline track */}
      <div className="absolute left-[11px] top-0 bottom-0 w-px bg-border/30" />

      <div className="flex flex-col">
        {messages.map((msg, idx) => {
          const config = ROLE_CONFIG[msg.role]
          const Icon = config.icon
          const dateStr = formatDate(msg.created_at)
          const showDateSeparator = dateStr !== lastDate
          lastDate = dateStr
          const isLast = idx === messages.length - 1
          const hasToolCalls = msg.tool_calls_json != null && msg.tool_calls_json !== '[]'

          return (
            <div key={msg.id}>
              {showDateSeparator && <DateMarker label={dateStr} />}

              <div className="group relative flex gap-3 py-1.5">
                {/* Timeline node */}
                <div className="relative z-10 flex shrink-0 flex-col items-center">
                  <div className={cn(
                    'flex h-[22px] w-[22px] items-center justify-center rounded-full ring-2',
                    config.nodeClass,
                  )}>
                    <Icon className="h-[10px] w-[10px] text-white" strokeWidth={2.5} />
                  </div>
                  {/* Connecting line below node (hidden for last) */}
                  {!isLast && (
                    <div className="w-px flex-1 bg-border/20" />
                  )}
                </div>

                {/* Content */}
                <div className="min-w-0 flex-1 pb-4">
                  {/* Header row */}
                  <div className="mb-1 flex items-center gap-2">
                    <span className={cn(
                      'font-mono text-[10px] font-semibold uppercase tracking-widest',
                      config.lineClass,
                    )}>
                      {config.label}
                    </span>
                    <span className="font-mono text-[9px] tabular-nums text-muted-foreground/30">
                      {formatTime(msg.created_at)}
                    </span>
                    {msg.role === 'tool' && msg.tool_call_id && (
                      <span className="font-mono text-[9px] text-muted-foreground/20">
                        {msg.tool_call_id.slice(0, 12)}
                      </span>
                    )}
                  </div>

                  {/* System messages: compact inline */}
                  {msg.role === 'system' ? (
                    <p className="font-mono text-[10px] leading-relaxed text-muted-foreground/40 line-clamp-2">
                      {msg.content}
                    </p>
                  ) : (
                    <>
                      {/* Message content */}
                      {msg.content && (
                        <div className="rounded-md bg-card/50 px-3 py-2 ring-1 ring-border/20">
                          <p className="whitespace-pre-wrap font-mono text-[11px] leading-relaxed text-foreground/80">
                            {msg.content}
                          </p>
                        </div>
                      )}

                      {/* Tool calls */}
                      {hasToolCalls && (
                        <div className={cn('mt-2', msg.content && 'mt-2.5')}>
                          <ToolCallBlock
                            toolCallsJson={msg.tool_calls_json}
                            result={msg.role === 'tool' ? msg.content : null}
                            durationMs={null}
                            isSuccess={true}
                          />
                        </div>
                      )}

                      {/* Tool result (for tool role messages without tool_calls) */}
                      {msg.role === 'tool' && !hasToolCalls && msg.content && (
                        <ToolCallBlock
                          toolCallsJson={null}
                          result={msg.content}
                          durationMs={null}
                          isSuccess={true}
                        />
                      )}
                    </>
                  )}
                </div>
              </div>
            </div>
          )
        })}
      </div>
    </div>
  )
}

function DateMarker({ label }: { label: string }) {
  return (
    <div className="relative my-3 flex items-center gap-3 pl-7">
      <div className="h-px flex-1 bg-border/20" />
      <span className="font-mono text-[9px] uppercase tracking-widest text-muted-foreground/30">
        {label}
      </span>
      <div className="h-px flex-1 bg-border/20" />
    </div>
  )
}

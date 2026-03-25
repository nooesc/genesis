import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import type { CommandMapNode } from '@/lib/command-map/types'

interface CommandMapInspectorProps {
  selectedNode: CommandMapNode | null
  onOpenRecipeDetails: (skillName: string) => void
  onOpenTriggerDetails: (scheduleId: string) => void
  onOpenThreadDetails: (sessionId: string) => void
  onOpenEventLog: (context: { title: string; sessionId?: string | null; eventType?: string | null }) => void
}

function buildFacts(selectedNode: CommandMapNode): Array<{ label: string; value: string }> {
  switch (selectedNode.kind) {
    case 'recipe':
      return [
        { label: 'Skill', value: String(selectedNode.data?.skill_name ?? selectedNode.id) },
        { label: 'Tags', value: String(selectedNode.data?.tag_count ?? 0) },
      ]
    case 'trigger':
      return [
        { label: 'Schedule', value: String(selectedNode.data?.schedule_id ?? selectedNode.id) },
        { label: 'Enabled', value: String(selectedNode.data?.enabled ?? false) },
      ]
    case 'thread':
      return [
        { label: 'Session', value: String(selectedNode.data?.session_id ?? selectedNode.id) },
        { label: 'Platform', value: String(selectedNode.data?.platform ?? 'unknown') },
      ]
    case 'alert':
      return [
        { label: 'Event', value: String(selectedNode.data?.event_type ?? selectedNode.label) },
        { label: 'Session', value: String(selectedNode.data?.session_id ?? 'global') },
      ]
    case 'system':
      return [
        { label: 'Model', value: String(selectedNode.data?.model ?? 'unknown') },
        { label: 'Ring', value: String(selectedNode.ring) },
      ]
    case 'eve':
      return [
        { label: 'Model', value: String(selectedNode.data?.model ?? 'unknown') },
        { label: 'Uptime', value: String(selectedNode.data?.uptime_seconds ?? 0) },
      ]
    default:
      return []
  }
}

export function CommandMapInspector({
  selectedNode,
  onOpenRecipeDetails,
  onOpenTriggerDetails,
  onOpenThreadDetails,
  onOpenEventLog,
}: CommandMapInspectorProps) {
  if (!selectedNode) {
    return (
      <Card className="h-full">
        <CardHeader className="pb-2">
          <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
            Inspector
          </CardTitle>
        </CardHeader>
        <CardContent className="font-mono text-sm text-muted-foreground/60">
          Select a node to inspect
        </CardContent>
      </Card>
    )
  }

  const facts = buildFacts(selectedNode)

  return (
    <Card className="h-full">
      <CardHeader className="pb-2">
        <CardTitle className="font-mono text-[10px] uppercase tracking-[0.2em] text-muted-foreground/60">
          Inspector
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <div className="space-y-1">
          <h2 className="font-mono text-base font-semibold text-foreground">{selectedNode.label}</h2>
          {selectedNode.subtitle && (
            <p className="font-mono text-xs text-muted-foreground/70">{selectedNode.subtitle}</p>
          )}
        </div>
        <div className="flex flex-wrap gap-2">
          <Badge variant="secondary" className="font-mono text-[10px] uppercase tracking-[0.18em]">
            {selectedNode.kind}
          </Badge>
          <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
            {selectedNode.layer}
          </Badge>
          <Badge variant="outline" className="font-mono text-[10px] uppercase tracking-[0.18em]">
            ring {selectedNode.ring}
          </Badge>
        </div>
        <dl className="grid grid-cols-2 gap-2 font-mono text-xs">
          <div className="rounded-lg border border-border/20 bg-background/40 p-2">
            <dt className="text-muted-foreground/50">ID</dt>
            <dd className="truncate text-foreground/80">{selectedNode.id}</dd>
          </div>
          <div className="rounded-lg border border-border/20 bg-background/40 p-2">
            <dt className="text-muted-foreground/50">Status</dt>
            <dd className="text-foreground/80">{selectedNode.status ?? 'unknown'}</dd>
          </div>
        </dl>
        {facts.length > 0 && (
          <dl className="grid grid-cols-2 gap-2 font-mono text-xs">
            {facts.map(fact => (
              <div key={fact.label} className="rounded-lg border border-border/20 bg-background/40 p-2">
                <dt className="text-muted-foreground/50">{fact.label}</dt>
                <dd className="truncate text-foreground/80">{fact.value}</dd>
              </div>
            ))}
          </dl>
        )}
        <div className="flex flex-wrap gap-2 pt-1">
          {selectedNode.kind === 'recipe' && (
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenRecipeDetails(String(selectedNode.data?.skill_name ?? selectedNode.id))}
              className="font-mono text-[11px] uppercase tracking-[0.18em]"
            >
              Recipe details
            </Button>
          )}
          {selectedNode.kind === 'trigger' && (
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenTriggerDetails(String(selectedNode.data?.schedule_id ?? selectedNode.id))}
              className="font-mono text-[11px] uppercase tracking-[0.18em]"
            >
              Trigger details
            </Button>
          )}
          {selectedNode.kind === 'thread' && (
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenThreadDetails(String(selectedNode.data?.session_id ?? selectedNode.id))}
              className="font-mono text-[11px] uppercase tracking-[0.18em]"
            >
              Thread details
            </Button>
          )}
          {(selectedNode.kind === 'eve' || selectedNode.kind === 'system' || selectedNode.kind === 'alert') && (
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                const sessionId = typeof selectedNode.data?.session_id === 'string' ? selectedNode.data.session_id : null
                const eventType = typeof selectedNode.data?.event_type === 'string' ? selectedNode.data?.event_type : null
                onOpenEventLog({
                  title:
                    selectedNode.kind === 'alert'
                      ? `Alert logs for ${selectedNode.label}`
                      : `${selectedNode.label} logs`,
                  sessionId: selectedNode.kind === 'alert' ? sessionId : null,
                  eventType: selectedNode.kind === 'alert' ? eventType : null,
                })
              }}
              className="font-mono text-[11px] uppercase tracking-[0.18em]"
            >
              Event log
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

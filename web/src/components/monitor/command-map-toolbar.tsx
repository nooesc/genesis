import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import type { CommandMapNodeLayer } from '@/lib/command-map/types'

const layers: { key: CommandMapNodeLayer; label: string }[] = [
  { key: 'core', label: 'Core' },
  { key: 'execution', label: 'Execution' },
  { key: 'trigger', label: 'Triggers' },
  { key: 'system', label: 'Systems' },
  { key: 'alert', label: 'Alerts' },
]

interface CommandMapToolbarProps {
  visibleLayers: Record<CommandMapNodeLayer, boolean>
  isDecluttered: boolean
  isFocused: boolean
  canFocus: boolean
  onToggleLayer: (layer: CommandMapNodeLayer) => void
  onToggleDeclutter: () => void
  onToggleFocus: () => void
  onReset: () => void
}

export function CommandMapToolbar({
  visibleLayers,
  isDecluttered,
  isFocused,
  canFocus,
  onToggleLayer,
  onToggleDeclutter,
  onToggleFocus,
  onReset,
}: CommandMapToolbarProps) {
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border/20 bg-card/40 p-3">
      {layers.map(({ key, label }) => (
        <Button
          key={key}
          type="button"
          variant={visibleLayers[key] ? 'default' : 'outline'}
          size="sm"
          onClick={() => onToggleLayer(key)}
          aria-pressed={visibleLayers[key]}
          className={cn(
            'font-mono text-[11px] uppercase tracking-[0.18em]',
            visibleLayers[key] ? 'shadow-sm' : 'text-muted-foreground/70',
          )}
        >
          {label}
        </Button>
      ))}

      <div className="ml-auto flex items-center gap-2">
        <Button
          type="button"
          variant={isDecluttered ? 'default' : 'outline'}
          size="sm"
          onClick={onToggleDeclutter}
          aria-pressed={isDecluttered}
          className="font-mono text-[11px] uppercase tracking-[0.18em]"
        >
          Declutter
        </Button>
        <Button
          type="button"
          variant={isFocused ? 'default' : 'outline'}
          size="sm"
          onClick={onToggleFocus}
          aria-pressed={isFocused}
          disabled={!canFocus}
          className="font-mono text-[11px] uppercase tracking-[0.18em]"
        >
          Focus
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={onReset} className="font-mono text-[11px] uppercase tracking-[0.18em]">
          Reset
        </Button>
      </div>
    </div>
  )
}

import { navRoutes } from '@/lib/nav'

interface ShortcutHelpProps {
  open: boolean
  onClose: () => void
}

export function ShortcutHelp({ open, onClose }: ShortcutHelpProps) {
  if (!open) return null

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50 backdrop-blur-sm"
      onClick={onClose}
      role="dialog"
      aria-label="Keyboard shortcuts"
    >
      <div
        className="w-full max-w-md rounded-lg border border-border/40 bg-[#0d0d0d] p-5 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="font-mono text-xs font-semibold uppercase tracking-widest text-foreground/80">
            Keyboard Shortcuts
          </h2>
          <span className="font-mono text-[9px] text-muted-foreground/40">
            Press ? to toggle
          </span>
        </div>

        <div className="space-y-4">
          {/* Navigation */}
          <Section title="Navigation">
            {navRoutes.filter(r => r.shortcut).map(r => (
              <Row key={r.to} keys={[r.shortcut!]} label={r.label} />
            ))}
          </Section>

          {/* Actions */}
          <Section title="Actions">
            <Row keys={['⌘', 'K']} label="Command palette" />
            <Row keys={['K']} label="Command palette (no modifier)" />
            <Row keys={['/']} label="Focus search" />
            <Row keys={['?']} label="This help" />
            <Row keys={['Esc']} label="Close overlay" />
          </Section>
        </div>
      </div>
    </div>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <h3 className="mb-2 font-mono text-[9px] uppercase tracking-widest text-muted-foreground/40">
        {title}
      </h3>
      <div className="space-y-1">{children}</div>
    </div>
  )
}

function Row({ keys, label }: { keys: string[]; label: string }) {
  return (
    <div className="flex items-center justify-between py-0.5">
      <span className="font-mono text-[11px] text-foreground/60">{label}</span>
      <div className="flex items-center gap-0.5">
        {keys.map((key, i) => (
          <kbd
            key={i}
            className="flex h-5 min-w-[20px] items-center justify-center rounded border border-border/40 bg-muted/30 px-1.5 font-mono text-[9px] text-foreground/50"
          >
            {key}
          </kbd>
        ))}
      </div>
    </div>
  )
}

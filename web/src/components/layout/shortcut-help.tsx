import { navRoutes } from '@/lib/nav'

interface ShortcutHelpProps {
  open: boolean
  onClose: () => void
}

export function ShortcutHelp({ open, onClose }: ShortcutHelpProps) {
  if (!open) return null

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-md"
      onClick={onClose}
      role="dialog"
      aria-label="Keyboard shortcuts"
    >
      <div
        className="page-enter w-full max-w-sm rounded-xl border border-border/30 bg-[#0b0b0b] p-6 shadow-2xl shadow-black/50"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header with gradient line */}
        <div className="mb-5 flex items-center justify-between">
          <h2 className="bg-gradient-to-r from-foreground to-muted-foreground bg-clip-text font-mono text-[11px] font-bold uppercase tracking-[0.15em] text-transparent">
            Keyboard Shortcuts
          </h2>
          <kbd className="flex items-center gap-0.5 rounded border border-border/30 bg-muted/10 px-1.5 py-0.5 font-mono text-[8px] text-muted-foreground/40">
            ? to toggle
          </kbd>
        </div>

        <div className="space-y-5">
          {/* Navigation */}
          <Section title="Navigation">
            {navRoutes.filter(r => r.shortcut).map(r => (
              <Row key={r.to} keys={[r.shortcut!]} label={r.label} />
            ))}
          </Section>

          {/* Actions */}
          <Section title="Actions">
            <Row keys={['⌘', 'K']} label="Command palette" />
            <Row keys={['K']} label="Quick command (no modifier)" />
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
      <div className="mb-2 flex items-center gap-2">
        <h3 className="font-mono text-[8px] uppercase tracking-widest text-muted-foreground/30">
          {title}
        </h3>
        <div className="section-divider h-px flex-1 opacity-15" />
      </div>
      <div className="space-y-0.5">{children}</div>
    </div>
  )
}

function Row({ keys, label }: { keys: string[]; label: string }) {
  return (
    <div className="flex items-center justify-between rounded px-1.5 py-1 transition-colors hover:bg-muted/10">
      <span className="font-mono text-[10px] text-foreground/50">{label}</span>
      <div className="flex items-center gap-0.5">
        {keys.map((key, i) => (
          <kbd
            key={i}
            className="flex h-5 min-w-[18px] items-center justify-center rounded border border-border/30 bg-muted/20 px-1.5 font-mono text-[8px] text-foreground/40"
          >
            {key}
          </kbd>
        ))}
      </div>
    </div>
  )
}

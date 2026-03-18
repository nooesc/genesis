import { useEffect, useId, useMemo, useState } from 'react'
import type { SessionSummary } from '@/lib/api/types'
import { getPlatformColor } from '@/lib/platforms'
import { formatTokens } from '@/lib/utils'

interface AgentCanvasProps {
  sessions: SessionSummary[]
  isHealthy: boolean
  totalTools: number
  uptimeSeconds: number
  toolUsage: [string, number][]
}

interface SessionNode {
  session: SessionSummary
  angle: number
  radius: number
  size: number
  tokens: number
  updatedAtMs: number
}

function layoutSessions(sessions: SessionSummary[]): SessionNode[] {
  const maxTokens = Math.max(1, ...sessions.map(s => s.total_input_tokens + s.total_output_tokens))

  return sessions.map((session, i) => {
    const tokens = session.total_input_tokens + session.total_output_tokens
    const angle = (i / sessions.length) * Math.PI * 2 - Math.PI / 2
    const recency = 1 - i / Math.max(1, sessions.length - 1)
    const radius = 110 + recency * 70
    const size = 3.5 + (tokens / maxTokens) * 8

    return { session, angle, radius, size, tokens, updatedAtMs: new Date(session.updated_at).getTime() }
  })
}

function getToolCategories(toolUsage: [string, number][], max: number = 10): [string, number][] {
  return [...toolUsage]
    .sort(([, a], [, b]) => b - a)
    .slice(0, max)
}

export function AgentCanvas({ sessions, isHealthy, totalTools, uptimeSeconds, toolUsage }: AgentCanvasProps) {
  const [tick, setTick] = useState(0)
  const uid = useId()

  const nodes = useMemo(() => layoutSessions(sessions.slice(0, 16)), [sessions])
  const tools = useMemo(() => getToolCategories(toolUsage), [toolUsage])
  const maxToolCount = tools.length > 0 ? tools[0][1] : 1

  // Animation loop with visibility gate — pauses when tab is hidden
  useEffect(() => {
    let lastTime = 0
    let rafId = 0

    function animate(time: number) {
      if (!document.hidden && time - lastTime > 33) {
        setTick((t: number) => t + 1)
        lastTime = time
      }
      rafId = requestAnimationFrame(animate)
    }

    function onVisibilityChange() {
      if (document.hidden) {
        cancelAnimationFrame(rafId)
      } else {
        rafId = requestAnimationFrame(animate)
      }
    }

    document.addEventListener('visibilitychange', onVisibilityChange)
    rafId = requestAnimationFrame(animate)

    return () => {
      cancelAnimationFrame(rafId)
      document.removeEventListener('visibilitychange', onVisibilityChange)
    }
  }, [])

  const W = 640
  const H = 520
  const cx = W / 2
  const cy = H / 2

  const rot = tick * 0.12
  const pulse = Math.sin(tick * 0.04)
  const sweepAngle = (tick * 1.8) % 360
  const coreR = 30 + pulse * 1.5
  const coreColor = isHealthy ? '#22c55e' : '#ef4444'
  const coreColorDim = isHealthy ? 'rgba(34,197,94,0.08)' : 'rgba(239,68,68,0.08)'

  const glowId = `${uid}-glow`
  const sweepId = `${uid}-sweep`
  const gridMask = `${uid}-grid`
  const nodeGlowId = `${uid}-nglow`

  return (
    <svg viewBox={`0 0 ${W} ${H}`} className="h-full w-full select-none" style={{ maxHeight: '100%' }}>
      <defs>
        {/* Core glow */}
        <radialGradient id={glowId}>
          <stop offset="0%" stopColor={coreColorDim} />
          <stop offset="100%" stopColor="transparent" />
        </radialGradient>

        {/* Radar sweep gradient */}
        <linearGradient id={sweepId} x1="0" y1="0" x2="1" y2="0">
          <stop offset="0%" stopColor={coreColor} stopOpacity={0} />
          <stop offset="100%" stopColor={coreColor} stopOpacity={0.12} />
        </linearGradient>

        {/* Node glow filter */}
        <filter id={nodeGlowId} x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur in="SourceGraphic" stdDeviation="2" />
        </filter>

        {/* Circular clip for sweep */}
        <clipPath id={gridMask}>
          <circle cx={cx} cy={cy} r={210} />
        </clipPath>
      </defs>

      {/* === LAYER 0: Crosshair grid === */}
      <g opacity={0.07} clipPath={`url(#${gridMask})`}>
        {/* Horizontal + vertical lines through center */}
        <line x1={cx - 220} y1={cy} x2={cx + 220} y2={cy} stroke="var(--foreground)" strokeWidth={0.5} />
        <line x1={cx} y1={cy - 220} x2={cx} y2={cy + 220} stroke="var(--foreground)" strokeWidth={0.5} />
        {/* Diagonal lines */}
        <line x1={cx - 156} y1={cy - 156} x2={cx + 156} y2={cy + 156} stroke="var(--foreground)" strokeWidth={0.3} />
        <line x1={cx + 156} y1={cy - 156} x2={cx - 156} y2={cy + 156} stroke="var(--foreground)" strokeWidth={0.3} />
      </g>

      {/* === LAYER 1: Orbit rings with tick marks === */}
      {[90, 130, 170, 210].map((r, ri) => (
        <g key={r}>
          <circle cx={cx} cy={cy} r={r} fill="none" stroke="var(--foreground)" strokeWidth={0.4} opacity={0.06 + ri * 0.01} />
          {/* Tick marks at cardinal points */}
          {[0, 90, 180, 270].map(deg => {
            const rad = (deg * Math.PI) / 180
            const inner = r - 3
            const outer = r + 3
            return (
              <line
                key={`${r}-${deg}`}
                x1={cx + inner * Math.cos(rad)}
                y1={cy + inner * Math.sin(rad)}
                x2={cx + outer * Math.cos(rad)}
                y2={cy + outer * Math.sin(rad)}
                stroke="var(--foreground)"
                strokeWidth={0.5}
                opacity={0.12}
              />
            )
          })}
        </g>
      ))}

      {/* === LAYER 2: Radar sweep === */}
      {(() => {
        const sweepRad = (sweepAngle * Math.PI) / 180
        const sweepLen = 200
        const sx = cx + sweepLen * Math.cos(sweepRad)
        const sy = cy + sweepLen * Math.sin(sweepRad)
        // Trailing arc (30 degrees behind sweep line)
        const trailStart = sweepAngle - 30
        const trailRad1 = (trailStart * Math.PI) / 180
        const tx1 = cx + sweepLen * Math.cos(trailRad1)
        const ty1 = cy + sweepLen * Math.sin(trailRad1)

        return (
          <g clipPath={`url(#${gridMask})`}>
            {/* Sweep trail arc */}
            <path
              d={`M ${cx} ${cy} L ${tx1} ${ty1} A ${sweepLen} ${sweepLen} 0 0 1 ${sx} ${sy} Z`}
              fill={coreColor}
              opacity={0.04}
            />
            {/* Sweep line */}
            <line x1={cx} y1={cy} x2={sx} y2={sy} stroke={coreColor} strokeWidth={0.6} opacity={0.2} />
          </g>
        )
      })()}

      {/* === LAYER 3: Tool gauge arcs (outer ring) === */}
      {tools.map(([name, count], i: number) => {
        const segmentAngle = 360 / Math.max(tools.length, 1)
        const gap = 3
        const arcSpan = segmentAngle - gap
        const fillFraction = count / maxToolCount
        const filledArc = arcSpan * fillFraction
        const startDeg = i * segmentAngle + rot + gap / 2

        const r = 205
        const trackR = r

        // Track (background)
        const tStart = (startDeg * Math.PI) / 180
        const tEnd = ((startDeg + arcSpan) * Math.PI) / 180
        const tx1 = cx + trackR * Math.cos(tStart)
        const ty1 = cy + trackR * Math.sin(tStart)
        const tx2 = cx + trackR * Math.cos(tEnd)
        const ty2 = cy + trackR * Math.sin(tEnd)
        const tLarge = arcSpan > 180 ? 1 : 0

        // Fill
        const fEnd = ((startDeg + filledArc) * Math.PI) / 180
        const fx2 = cx + trackR * Math.cos(fEnd)
        const fy2 = cy + trackR * Math.sin(fEnd)
        const fLarge = filledArc > 180 ? 1 : 0

        // Label
        const midRad = ((startDeg + arcSpan / 2) * Math.PI) / 180
        const lx = cx + (r + 18) * Math.cos(midRad)
        const ly = cy + (r + 18) * Math.sin(midRad)
        // Rotate label to follow arc
        const labelAngle = startDeg + arcSpan / 2

        return (
          <g key={name}>
            {/* Track */}
            <path
              d={`M ${tx1} ${ty1} A ${trackR} ${trackR} 0 ${tLarge} 1 ${tx2} ${ty2}`}
              fill="none"
              stroke="var(--foreground)"
              strokeWidth={3}
              opacity={0.04}
              strokeLinecap="round"
            />
            {/* Fill */}
            <path
              d={`M ${tx1} ${ty1} A ${trackR} ${trackR} 0 ${fLarge} 1 ${fx2} ${fy2}`}
              fill="none"
              stroke="#0891b2"
              strokeWidth={2.5}
              strokeLinecap="round"
              opacity={0.25 + fillFraction * 0.45}
            />
            {/* Label */}
            <text
              x={lx}
              y={ly}
              textAnchor="middle"
              dominantBaseline="middle"
              fill="var(--muted-foreground)"
              fontSize={6.5}
              fontFamily="var(--font-mono)"
              opacity={0.4}
              transform={`rotate(${labelAngle > 90 && labelAngle < 270 ? labelAngle + 180 : labelAngle} ${lx} ${ly})`}
            >
              {name.length > 12 ? name.slice(0, 11) + '…' : name}
            </text>
          </g>
        )
      })}

      {/* === LAYER 4: Session connections + nodes === */}
      {(() => {
        const nowMs = Date.now()
        return nodes.map(({ session, angle, radius, size, tokens, updatedAtMs }) => {
        const a = angle + (rot * Math.PI) / 180
        const nx = cx + radius * Math.cos(a)
        const ny = cy + radius * Math.sin(a)
        const color = getPlatformColor(session.platform)

        const ageMs = nowMs - updatedAtMs
        const ageHours = ageMs / (1000 * 60 * 60)
        const nodeOpacity = Math.max(0.15, 1 - ageHours / 72)
        const isRecent = ageHours < 2

        // Data pulse position along connection (animated)
        const pulsePos = ((tick * 0.8 + angle * 100) % 100) / 100
        const px = cx + (nx - cx) * pulsePos
        const py = cy + (ny - cy) * pulsePos

        return (
          <g key={session.id} opacity={nodeOpacity}>
            {/* Connection line — dashed, subtle */}
            <line
              x1={cx} y1={cy} x2={nx} y2={ny}
              stroke={color}
              strokeWidth={0.4}
              strokeDasharray="2 4"
              opacity={0.12}
            />

            {/* Data pulse traveling along connection */}
            {isRecent && (
              <circle cx={px} cy={py} r={1.2} fill={color} opacity={0.5} />
            )}

            {/* Outer ring (scanner ring) */}
            <circle cx={nx} cy={ny} r={size + 3} fill="none" stroke={color} strokeWidth={0.4} opacity={0.2} />

            {/* Node glow (filtered) — only for recent */}
            {isRecent && (
              <circle cx={nx} cy={ny} r={size + 1} fill={color} opacity={0.15} filter={`url(#${nodeGlowId})`} />
            )}

            {/* Node core */}
            <circle cx={nx} cy={ny} r={size} fill={color} opacity={0.65} />
            {/* Inner dot */}
            <circle cx={nx} cy={ny} r={1.5} fill="#ffffff" opacity={0.5} />

            {/* Bracket-style HUD label */}
            <g opacity={0.45}>
              {/* Left bracket */}
              <path
                d={`M ${nx - size - 6} ${ny + size + 5} l 2 0 l 0 4`}
                fill="none" stroke="var(--foreground)" strokeWidth={0.5}
              />
              {/* Right bracket */}
              <path
                d={`M ${nx + size + 6} ${ny + size + 5} l -2 0 l 0 4`}
                fill="none" stroke="var(--foreground)" strokeWidth={0.5}
              />
              {/* ID */}
              <text
                x={nx} y={ny + size + 10}
                textAnchor="middle"
                fill="var(--foreground)"
                fontSize={6}
                fontFamily="var(--font-mono)"
                letterSpacing="0.5"
              >
                {session.id.slice(0, 6)}
              </text>
              {/* Token count below */}
              <text
                x={nx} y={ny + size + 17}
                textAnchor="middle"
                fill="var(--muted-foreground)"
                fontSize={5}
                fontFamily="var(--font-mono)"
                opacity={0.6}
              >
                {formatTokens(tokens)}
              </text>
            </g>

            {/* Pulse ring for very recent sessions */}
            {ageHours < 0.5 && (
              <circle
                cx={nx} cy={ny}
                r={size + 5 + pulse * 2}
                fill="none" stroke={color} strokeWidth={0.5}
                opacity={0.15 + pulse * 0.1}
              />
            )}
          </g>
        )
      })
      })()}

      {/* === LAYER 5: Central core — Eve === */}
      {/* Ambient glow */}
      <circle cx={cx} cy={cy} r={65} fill={`url(#${glowId})`} />

      {/* Outer ring with segments */}
      {[0, 60, 120, 180, 240, 300].map(deg => {
        const rad1 = ((deg + 5) * Math.PI) / 180
        const rad2 = ((deg + 55) * Math.PI) / 180
        const r = coreR + 4
        const x1 = cx + r * Math.cos(rad1)
        const y1 = cy + r * Math.sin(rad1)
        const x2 = cx + r * Math.cos(rad2)
        const y2 = cy + r * Math.sin(rad2)
        return (
          <path
            key={deg}
            d={`M ${x1} ${y1} A ${r} ${r} 0 0 1 ${x2} ${y2}`}
            fill="none"
            stroke={coreColor}
            strokeWidth={1}
            opacity={0.3 + pulse * 0.1}
          />
        )
      })}

      {/* Inner rings */}
      <circle cx={cx} cy={cy} r={coreR} fill="none" stroke={coreColor} strokeWidth={1.2} opacity={0.5} />
      <circle cx={cx} cy={cy} r={coreR - 7} fill="none" stroke={coreColor} strokeWidth={0.4} opacity={0.2} strokeDasharray="3 3" />

      {/* Crosshair inside core */}
      <line x1={cx - 8} y1={cy} x2={cx - 3} y2={cy} stroke={coreColor} strokeWidth={0.5} opacity={0.3} />
      <line x1={cx + 3} y1={cy} x2={cx + 8} y2={cy} stroke={coreColor} strokeWidth={0.5} opacity={0.3} />
      <line x1={cx} y1={cy - 8} x2={cx} y2={cy - 3} stroke={coreColor} strokeWidth={0.5} opacity={0.3} />
      <line x1={cx} y1={cy + 3} x2={cx} y2={cy + 8} stroke={coreColor} strokeWidth={0.5} opacity={0.3} />

      {/* Core label */}
      <text x={cx} y={cy - 1} textAnchor="middle" dominantBaseline="middle" fill={coreColor} fontSize={11} fontFamily="var(--font-mono)" fontWeight="600" letterSpacing="4" opacity={0.9}>
        EVE
      </text>

      {/* Uptime arc (270-degree gauge) */}
      {(() => {
        const maxUp = 30 * 86400
        const frac = Math.min(uptimeSeconds / maxUp, 1)
        const r = 42
        const circ = 2 * Math.PI * r
        const arcLen = circ * 0.75
        const dash = arcLen * frac
        return (
          <circle
            cx={cx} cy={cy} r={r}
            fill="none" stroke={coreColor} strokeWidth={1.5}
            strokeDasharray={`${dash} ${circ}`}
            strokeLinecap="round"
            opacity={0.2}
            transform={`rotate(135 ${cx} ${cy})`}
          />
        )
      })()}

      {/* HUD readouts at corners */}
      <g opacity={0.3} fontFamily="var(--font-mono)" fontSize={7} fill="var(--foreground)">
        {/* Top-left: session count */}
        <text x={16} y={18}>
          <tspan fill="var(--muted-foreground)" fontSize={5.5}>SES </tspan>
          <tspan>{String(sessions.length).padStart(3, '0')}</tspan>
        </text>
        {/* Top-right: tool count */}
        <text x={W - 16} y={18} textAnchor="end">
          <tspan fill="var(--muted-foreground)" fontSize={5.5}>TOOLS </tspan>
          <tspan>{String(totalTools).padStart(3, '0')}</tspan>
        </text>
        {/* Bottom-left: status */}
        <text x={16} y={H - 10}>
          <tspan fill={coreColor} fontSize={5.5}>{isHealthy ? 'NOMINAL' : 'DEGRADED'}</tspan>
        </text>
        {/* Bottom-right: uptime */}
        <text x={W - 16} y={H - 10} textAnchor="end">
          <tspan fill="var(--muted-foreground)" fontSize={5.5}>UP </tspan>
          <tspan>{formatUptime(uptimeSeconds)}</tspan>
        </text>
      </g>

      {/* Corner brackets (HUD frame) */}
      <g stroke="var(--foreground)" strokeWidth={0.5} opacity={0.08} fill="none">
        <path d="M 6 20 L 6 6 L 20 6" />
        <path d={`M ${W - 6} 20 L ${W - 6} 6 L ${W - 20} 6`} />
        <path d={`M 6 ${H - 20} L 6 ${H - 6} L 20 ${H - 6}`} />
        <path d={`M ${W - 6} ${H - 20} L ${W - 6} ${H - 6} L ${W - 20} ${H - 6}`} />
      </g>
    </svg>
  )
}

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  if (d > 0) return `${d}D ${String(h).padStart(2, '0')}H`
  if (h > 0) return `${h}H ${String(m).padStart(2, '0')}M`
  return `${m}M`
}

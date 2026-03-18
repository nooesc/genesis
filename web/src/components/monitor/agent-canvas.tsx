import { useEffect, useRef, useState } from 'react'
import type { SessionSummary } from '@/lib/api/types'

interface AgentCanvasProps {
  sessions: SessionSummary[]
  isHealthy: boolean
  totalTools: number
  uptimeSeconds: number
  toolUsage: Record<string, number>
}

interface SessionNode {
  session: SessionSummary
  angle: number
  radius: number
  size: number
}

// Distribute sessions in orbits around center
function layoutSessions(sessions: SessionSummary[]): SessionNode[] {
  const maxTokens = Math.max(1, ...sessions.map(s => s.total_input_tokens + s.total_output_tokens))

  return sessions.map((session, i) => {
    const tokens = session.total_input_tokens + session.total_output_tokens
    const angle = (i / sessions.length) * Math.PI * 2 - Math.PI / 2
    // Outer orbit for older/smaller sessions, inner for newer/larger
    const recency = 1 - i / Math.max(1, sessions.length - 1)
    const radius = 100 + recency * 60
    const size = 4 + (tokens / maxTokens) * 10

    return { session, angle, radius, size }
  })
}

// Top tool categories for the outer ring
function getToolCategories(toolUsage: Record<string, number>, max: number = 8): [string, number][] {
  return Object.entries(toolUsage)
    .sort(([, a], [, b]) => b - a)
    .slice(0, max)
}

const PLATFORM_COLORS: Record<string, string> = {
  api: '#0891b2',
  telegram: '#2563eb',
  discord: '#7c3aed',
  slack: '#dc2626',
  whatsapp: '#16a34a',
  homeassistant: '#f59e0b',
}

function getPlatformColor(platform: string): string {
  return PLATFORM_COLORS[platform.toLowerCase()] ?? '#525252'
}

export function AgentCanvas({ sessions, isHealthy, totalTools, uptimeSeconds, toolUsage }: AgentCanvasProps) {
  const [tick, setTick] = useState(0)
  const rafRef = useRef<number>(0)

  // Slow ambient animation (~30fps)
  useEffect(() => {
    let lastTime = 0
    function animate(time: number) {
      if (time - lastTime > 33) {
        setTick(t => t + 1)
        lastTime = time
      }
      rafRef.current = requestAnimationFrame(animate)
    }
    rafRef.current = requestAnimationFrame(animate)
    return () => cancelAnimationFrame(rafRef.current)
  }, [])

  const width = 600
  const height = 500
  const cx = width / 2
  const cy = height / 2

  const nodes = layoutSessions(sessions.slice(0, 16))
  const tools = getToolCategories(toolUsage)
  const maxToolCount = tools.length > 0 ? tools[0][1] : 1

  // Subtle rotation
  const rotation = tick * 0.15

  // Health pulse
  const pulsePhase = Math.sin(tick * 0.05)
  const coreRadius = 28 + pulsePhase * 2
  const coreGlow = isHealthy
    ? `rgba(34,197,94,${0.15 + pulsePhase * 0.1})`
    : `rgba(239,68,68,${0.15 + pulsePhase * 0.1})`
  const coreColor = isHealthy ? '#22c55e' : '#ef4444'

  return (
    <svg
      viewBox={`0 0 ${width} ${height}`}
      className="h-full w-full"
      style={{ maxHeight: '100%' }}
    >
      <defs>
        {/* Radial gradient for core glow */}
        <radialGradient id="core-glow" cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor={coreGlow} />
          <stop offset="100%" stopColor="transparent" />
        </radialGradient>

        {/* Gradient for orbit rings */}
        <radialGradient id="orbit-fade" cx="50%" cy="50%" r="50%">
          <stop offset="60%" stopColor="transparent" />
          <stop offset="100%" stopColor="rgba(8,145,178,0.03)" />
        </radialGradient>
      </defs>

      {/* Background gradient */}
      <circle cx={cx} cy={cy} r={240} fill="url(#orbit-fade)" />

      {/* Orbit rings */}
      {[100, 130, 160].map(r => (
        <circle
          key={r}
          cx={cx}
          cy={cy}
          r={r}
          fill="none"
          stroke="var(--border)"
          strokeWidth={0.5}
          strokeDasharray="4 8"
          opacity={0.3}
        />
      ))}

      {/* Tool category arcs on outer ring */}
      {tools.map(([name, count], i) => {
        const arcAngle = (count / maxToolCount) * 30
        const startAngle = (i / tools.length) * 360 + rotation
        const r = 200
        const startRad = (startAngle * Math.PI) / 180
        const endRad = ((startAngle + arcAngle) * Math.PI) / 180
        const x1 = cx + r * Math.cos(startRad)
        const y1 = cy + r * Math.sin(startRad)
        const x2 = cx + r * Math.cos(endRad)
        const y2 = cy + r * Math.sin(endRad)
        const largeArc = arcAngle > 180 ? 1 : 0

        // Label position
        const midRad = ((startAngle + arcAngle / 2) * Math.PI) / 180
        const lx = cx + (r + 16) * Math.cos(midRad)
        const ly = cy + (r + 16) * Math.sin(midRad)

        return (
          <g key={name} opacity={0.6}>
            <path
              d={`M ${x1} ${y1} A ${r} ${r} 0 ${largeArc} 1 ${x2} ${y2}`}
              fill="none"
              stroke="#0891b2"
              strokeWidth={2}
              strokeLinecap="round"
              opacity={0.4 + (count / maxToolCount) * 0.4}
            />
            <text
              x={lx}
              y={ly}
              textAnchor="middle"
              dominantBaseline="middle"
              fill="var(--muted-foreground)"
              fontSize={7}
              fontFamily="var(--font-mono)"
              opacity={0.5}
            >
              {name.length > 10 ? name.slice(0, 10) : name}
            </text>
          </g>
        )
      })}

      {/* Session nodes with connections to core */}
      {nodes.map(({ session, angle, radius, size }) => {
        const adjustedAngle = angle + (rotation * Math.PI) / 180
        const nx = cx + radius * Math.cos(adjustedAngle)
        const ny = cy + radius * Math.sin(adjustedAngle)
        const color = getPlatformColor(session.platform)

        // Age-based opacity (newer = brighter)
        const ageMs = Date.now() - new Date(session.updated_at).getTime()
        const ageHours = ageMs / (1000 * 60 * 60)
        const opacity = Math.max(0.2, 1 - ageHours / 72)

        return (
          <g key={session.id} opacity={opacity}>
            {/* Connection line to core */}
            <line
              x1={cx}
              y1={cy}
              x2={nx}
              y2={ny}
              stroke={color}
              strokeWidth={0.5}
              opacity={0.15}
            />
            {/* Node */}
            <circle
              cx={nx}
              cy={ny}
              r={size}
              fill={color}
              opacity={0.7}
            />
            {/* Glow for recent sessions */}
            {ageHours < 1 && (
              <circle
                cx={nx}
                cy={ny}
                r={size + 3}
                fill="none"
                stroke={color}
                strokeWidth={1}
                opacity={0.3 + pulsePhase * 0.2}
              />
            )}
            {/* Session ID label */}
            <text
              x={nx}
              y={ny + size + 8}
              textAnchor="middle"
              fill="var(--muted-foreground)"
              fontSize={7}
              fontFamily="var(--font-mono)"
              opacity={0.5}
            >
              {session.id.slice(0, 6)}
            </text>
          </g>
        )
      })}

      {/* Central core — Eve */}
      <circle cx={cx} cy={cy} r={60} fill="url(#core-glow)" />
      <circle
        cx={cx}
        cy={cy}
        r={coreRadius}
        fill="none"
        stroke={coreColor}
        strokeWidth={1.5}
        opacity={0.6}
      />
      <circle
        cx={cx}
        cy={cy}
        r={coreRadius - 6}
        fill="none"
        stroke={coreColor}
        strokeWidth={0.5}
        opacity={0.3}
      />

      {/* Core label */}
      <text
        x={cx}
        y={cy - 4}
        textAnchor="middle"
        dominantBaseline="middle"
        fill={coreColor}
        fontSize={14}
        fontFamily="var(--font-mono)"
        fontWeight="bold"
        letterSpacing="3"
      >
        EVE
      </text>
      <text
        x={cx}
        y={cy + 10}
        textAnchor="middle"
        dominantBaseline="middle"
        fill="var(--muted-foreground)"
        fontSize={8}
        fontFamily="var(--font-mono)"
        opacity={0.6}
      >
        {totalTools} tools
      </text>

      {/* Uptime ring indicator */}
      {(() => {
        const maxUptime = 30 * 86400 // 30 days
        const fraction = Math.min(uptimeSeconds / maxUptime, 1)
        const r = 38
        const circumference = 2 * Math.PI * r
        const dashLen = circumference * fraction

        return (
          <circle
            cx={cx}
            cy={cy}
            r={r}
            fill="none"
            stroke={coreColor}
            strokeWidth={1}
            strokeDasharray={`${dashLen} ${circumference}`}
            strokeLinecap="round"
            opacity={0.25}
            transform={`rotate(-90 ${cx} ${cy})`}
          />
        )
      })()}
    </svg>
  )
}

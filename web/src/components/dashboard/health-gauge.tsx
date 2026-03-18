import { useEffect, useState } from 'react'

interface HealthGaugeProps {
  /** 0–1 normalized value */
  value: number
  label: string
  status: 'success' | 'warning' | 'error'
  sublabel?: string
}

const STATUS_COLORS = {
  success: { stroke: '#22c55e', glow: 'rgba(34,197,94,0.3)' },
  warning: { stroke: '#eab308', glow: 'rgba(234,179,8,0.3)' },
  error: { stroke: '#ef4444', glow: 'rgba(239,68,68,0.3)' },
} as const

export function HealthGauge({ value, label, status, sublabel }: HealthGaugeProps) {
  const [animatedValue, setAnimatedValue] = useState(0)
  const colors = STATUS_COLORS[status]

  useEffect(() => {
    // Animate from 0 to value on mount
    const timer = setTimeout(() => setAnimatedValue(value), 50)
    return () => clearTimeout(timer)
  }, [value])

  const size = 80
  const strokeWidth = 4
  const radius = (size - strokeWidth) / 2
  const circumference = 2 * Math.PI * radius
  // Arc covers 270 degrees (3/4 of circle)
  const arcLength = circumference * 0.75
  const offset = arcLength * (1 - animatedValue)

  return (
    <div className="flex flex-col items-center gap-1">
      <div className="relative" style={{ width: size, height: size }}>
        <svg
          width={size}
          height={size}
          style={{ transform: 'rotate(135deg)' }}
        >
          {/* Background arc */}
          <circle
            cx={size / 2}
            cy={size / 2}
            r={radius}
            fill="none"
            stroke="var(--border)"
            strokeWidth={strokeWidth}
            strokeLinecap="round"
            strokeDasharray={`${arcLength} ${circumference}`}
          />
          {/* Value arc */}
          <circle
            cx={size / 2}
            cy={size / 2}
            r={radius}
            fill="none"
            stroke={colors.stroke}
            strokeWidth={strokeWidth}
            strokeLinecap="round"
            strokeDasharray={`${arcLength} ${circumference}`}
            strokeDashoffset={offset}
            style={{
              transition: 'stroke-dashoffset 1s cubic-bezier(0.34, 1.56, 0.64, 1)',
              filter: `drop-shadow(0 0 4px ${colors.glow})`,
            }}
          />
        </svg>
        {/* Center label */}
        <div className="absolute inset-0 flex flex-col items-center justify-center">
          <span
            className="font-mono text-lg font-bold tabular-nums"
            style={{ color: colors.stroke }}
          >
            {label}
          </span>
        </div>
      </div>
      {sublabel && (
        <span className="font-mono text-[9px] uppercase tracking-wider text-muted-foreground/60">
          {sublabel}
        </span>
      )}
    </div>
  )
}

import {
  BarChart,
  Bar,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from 'recharts'
import type { InsightsData } from '@/lib/api/types'

interface TokenChartProps {
  tokensPerDay: InsightsData['tokens_per_day']
}

interface ChartDataPoint {
  date: string
  tokens: number
}

function formatDateLabel(dateStr: string): string {
  try {
    const d = new Date(dateStr)
    return d.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
  } catch {
    return dateStr
  }
}

export function TokenChart({ tokensPerDay }: TokenChartProps) {
  // tokensPerDay is [date, input_tokens, output_tokens][]
  const data: ChartDataPoint[] = [...tokensPerDay]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([date, inp, out]) => ({
      date: formatDateLabel(date),
      tokens: inp + out,
    }))

  if (data.length === 0) {
    return (
      <div className="flex h-[200px] items-center justify-center font-mono text-xs text-muted-foreground">
        No token data available
      </div>
    )
  }

  return (
    <ResponsiveContainer width="100%" height={200}>
      <BarChart data={data} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
        <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" vertical={false} />
        <XAxis
          dataKey="date"
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
        />
        <YAxis
          tick={{ fontFamily: 'var(--font-geist-mono, monospace)', fontSize: 10 }}
          tickLine={false}
          axisLine={false}
          stroke="hsl(var(--muted-foreground))"
          tickFormatter={(v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}k` : String(v))}
          width={36}
        />
        <Tooltip
          cursor={{ fill: 'hsl(var(--accent))' }}
          contentStyle={{
            backgroundColor: 'hsl(var(--popover))',
            border: '1px solid hsl(var(--border))',
            borderRadius: '6px',
            fontFamily: 'var(--font-geist-mono, monospace)',
            fontSize: '11px',
            color: 'hsl(var(--popover-foreground))',
          }}
          labelStyle={{ color: 'hsl(var(--muted-foreground))' }}
          itemStyle={{ color: '#0891b2' }}
        />
        <Bar dataKey="tokens" name="Tokens" fill="#0891b2" radius={[2, 2, 0, 0]} />
      </BarChart>
    </ResponsiveContainer>
  )
}

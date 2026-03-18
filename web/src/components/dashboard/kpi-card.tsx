import { Card, CardContent } from '@/components/ui/card'

interface KpiCardProps {
  label: string
  value: string | number
  subtitle?: string
  status?: 'success' | 'warning' | 'error'
}

export function KpiCard({ label, value, subtitle, status }: KpiCardProps) {
  const statusColor = {
    success: 'text-green-500',
    warning: 'text-amber-500',
    error: 'text-red-500',
  }[status ?? 'success']

  return (
    <Card>
      <CardContent className="p-4">
        <div className="font-mono text-[10px] uppercase tracking-widest text-muted-foreground">
          {label}
        </div>
        <div className={`mt-1 font-mono text-xl font-bold ${status ? statusColor : 'text-foreground'}`}>
          {value}
        </div>
        {subtitle && (
          <div className="mt-0.5 font-mono text-[10px] text-muted-foreground">{subtitle}</div>
        )}
      </CardContent>
    </Card>
  )
}

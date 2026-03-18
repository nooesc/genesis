import {
  LayoutDashboard, MessagesSquare, Brain, Database,
  Clock, Wrench, BarChart3, FileText, Settings,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

export interface NavRoute {
  to: string
  label: string
  icon: LucideIcon
  keywords: string
}

export const navRoutes: NavRoute[] = [
  { to: '/', label: 'Dashboard', icon: LayoutDashboard, keywords: 'home overview kpi' },
  { to: '/sessions', label: 'Sessions', icon: MessagesSquare, keywords: 'chat conversations messages' },
  { to: '/skills', label: 'Skills', icon: Brain, keywords: 'abilities prompts templates' },
  { to: '/memories', label: 'Memories', icon: Database, keywords: 'knowledge storage recall' },
  { to: '/schedules', label: 'Schedules', icon: Clock, keywords: 'cron timer jobs automation' },
  { to: '/tools', label: 'Tools', icon: Wrench, keywords: 'functions registry mcp' },
  { to: '/analytics', label: 'Analytics', icon: BarChart3, keywords: 'stats usage tokens charts' },
  { to: '/audit', label: 'Audit Log', icon: FileText, keywords: 'history actions log events' },
  { to: '/settings', label: 'Settings', icon: Settings, keywords: 'config preferences' },
]

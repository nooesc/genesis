import {
  LayoutDashboard, MessagesSquare, Brain, Database,
  Clock, Wrench, BarChart3, FileText, Settings, Radar,
} from 'lucide-react'
import type { LucideIcon } from 'lucide-react'

export interface NavRoute {
  to: string
  label: string
  icon: LucideIcon
  keywords: string
  /** Keyboard shortcut key (shown in dock tooltip and command palette) */
  shortcut?: string
}

export const navRoutes: NavRoute[] = [
  { to: '/', label: 'Dashboard', icon: LayoutDashboard, keywords: 'home overview kpi', shortcut: '1' },
  { to: '/monitor', label: 'Monitor', icon: Radar, keywords: 'canvas agent observability radar', shortcut: '2' },
  { to: '/sessions', label: 'Sessions', icon: MessagesSquare, keywords: 'chat conversations messages', shortcut: '3' },
  { to: '/skills', label: 'Skills', icon: Brain, keywords: 'abilities prompts templates', shortcut: '4' },
  { to: '/memories', label: 'Memories', icon: Database, keywords: 'knowledge storage recall', shortcut: '5' },
  { to: '/schedules', label: 'Schedules', icon: Clock, keywords: 'cron timer jobs automation', shortcut: '6' },
  { to: '/tools', label: 'Tools', icon: Wrench, keywords: 'functions registry mcp', shortcut: '7' },
  { to: '/analytics', label: 'Analytics', icon: BarChart3, keywords: 'stats usage tokens charts', shortcut: '8' },
  { to: '/audit', label: 'Audit Log', icon: FileText, keywords: 'history actions log events', shortcut: '9' },
  { to: '/settings', label: 'Settings', icon: Settings, keywords: 'config preferences', shortcut: '0' },
]

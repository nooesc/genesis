// API response types for Genesis dashboard

export interface HealthResponse {
  status: string
  version: string
  uptime_seconds: number
  model: string
  mcp_servers: number
  active_schedules: number
  total_sessions: number
  total_tools: number
}

export interface McpServerStatus {
  name: string
  connected: boolean
  tool_count: number
  error?: string
}

export interface McpStatusResponse {
  servers: McpServerStatus[]
}

export interface MetricsJsonResponse {
  uptime_seconds: number
  requests_total: number
  errors_total: number
  input_tokens_total: number
  output_tokens_total: number
  stream_requests_total: number
  total_sessions: number
  active_schedules: number
}

export interface SessionSummary {
  id: string
  title: string | null
  platform: string
  total_input_tokens: number
  total_output_tokens: number
  parent_session_id: string | null
  created_at: string
  updated_at: string
}

export interface SessionsResponse {
  sessions: SessionSummary[]
  count: number
}

export interface StoredMessage {
  id: string
  session_id: string
  role: 'user' | 'assistant' | 'tool' | 'system'
  content: string | null
  tool_call_id: string | null
  tool_calls_json: string | null
  created_at: string
}

export interface Skill {
  name: string
  description: string
  content: string
  tags: string[]
  created_at: string
  updated_at: string
}

export interface SkillUsageStats {
  total_uses: number
  last_used_at: string | null
  avg_duration_ms: number | null
}

export interface SkillUsageRecord {
  id: string
  skill_name: string
  session_id: string | null
  duration_ms: number | null
  created_at: string
}

export interface Memory {
  id: string
  content: string
  source: string
  created_at: string
}

export interface Schedule {
  id: string
  cron_expression: string
  destination: string
  prompt: string
  enabled: boolean
  created_at: string
  last_run_at: string | null
}

export interface InsightsData {
  period_days: number
  sessions_count: number
  total_input_tokens: number
  total_output_tokens: number
  sessions_per_day: Record<string, number>
  platform_breakdown: Record<string, number>
  tokens_per_day: Record<string, number>
  tool_usage: Record<string, number>
  avg_input_tokens: number
  avg_output_tokens: number
}

export interface UsageStats {
  total_sessions: number
  total_input_tokens: number
  total_output_tokens: number
}

export interface AuditEntry {
  id: string
  action: string
  session_id: string | null
  details: string | null
  created_at: string
}

export interface ToolParameter {
  type: string
  description?: string
  enum?: string[]
  items?: ToolParameter
  properties?: Record<string, ToolParameter>
  required?: string[]
}

export interface ToolInfo {
  name: string
  description: string
  source: string
  parameters: {
    type: string
    properties: Record<string, ToolParameter>
    required?: string[]
  }
}

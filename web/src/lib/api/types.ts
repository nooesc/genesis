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
}

export interface McpStatusResponse {
  servers: McpServerStatus[]
  total_tools: number
  total_resources: number
  total_prompts: number
}

export interface CacheStatsResponse {
  enabled: boolean
  entries: number
  total_hits: number
}

export interface WebhookStatusResponse {
  configured: boolean
  delivered: number
  retried: number
  failed: number
  dead_letter_count: number
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
  /** [date, count] tuples — Rust Vec<(String, u64)> */
  sessions_per_day: [string, number][]
  /** [platform, count] tuples — Rust Vec<(String, u64)> */
  platform_breakdown: [string, number][]
  /** [date, input_tokens, output_tokens] tuples — Rust Vec<(String, u64, u64)> */
  tokens_per_day: [string, number, number][]
  /** [tool_name, count] tuples — Rust Vec<(String, u64)> */
  tool_usage: [string, number][]
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
  event_type: string
  session_id: string | null
  details: string | null
  created_at: string
}

export interface ApprovedUser {
  platform: string
  user_id: string
  user_name: string
  approved_at: string
}

export interface PendingPairing {
  platform: string
  code: string
  user_id: string
  user_name: string
  created_at: string
}

export interface ApprovedListResponse {
  approved: ApprovedUser[]
  count: number
}

export interface PendingListResponse {
  pending: PendingPairing[]
  count: number
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

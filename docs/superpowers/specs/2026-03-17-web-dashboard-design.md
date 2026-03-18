# Genesis Web Dashboard — Design Spec

**Date:** 2026-03-17
**Status:** Approved
**Author:** Cole + Claude

## Overview

A web-based admin/observability dashboard for Genesis, embedded in the binary and served by the existing Axum gateway. Provides health monitoring, session browsing, skill/memory/schedule management, analytics, and audit log viewing. No chat interface in v1.

Primary audience: the Genesis operator (personal use), but built with enough polish to ship as part of the project for any self-hoster.

## Tech Stack

| Layer | Choice | Rationale |
|-------|--------|-----------|
| Framework | React 19 + TypeScript | Largest ecosystem for dashboards, best component/charting libraries |
| Build | Vite | Fast HMR, simple config, builds to static files |
| Router | TanStack Router | Type-safe routes + search params, loader-based data prefetching |
| Data fetching | TanStack Query | Caching, polling, mutation invalidation, retry |
| Tables | TanStack Table | Sorting, filtering, pagination for data-heavy views |
| Components | Shadcn/ui | Copy-paste ownership, Tailwind-based, easy to theme |
| Charts | Recharts | Composable, good React integration |
| Styling | Tailwind CSS | Utility-first, pairs with Shadcn |
| Embedding | rust-embed | Compiles web/dist/ into the binary |
| Aesthetic | Terminal/hacker | Dark theme, monospace accents, dense data, Grafana/Vercel vibe |

## Architecture

```
┌─────────────────────────────────────────────────┐
│  genesis binary (single executable)             │
│                                                 │
│  ┌──────────────────────────────────────────┐   │
│  │ genesis-gateway (Axum)                   │   │
│  │                                          │   │
│  │  /api/*    → existing endpoints (nested) │   │
│  │  /chat/ws  → existing WebSocket          │   │
│  │  /*        → rust-embed static files     │   │
│  │            → SPA fallback to index.html  │   │
│  └──────────────────────────────────────────┘   │
│                                                 │
│  Static assets compiled in via rust-embed       │
│  from web/dist/ at build time                   │
└─────────────────────────────────────────────────┘
```

- `web/` directory in monorepo root holds the React app
- `cargo build` triggers `npm run build` first (via justfile/Makefile)
- `rust-embed` embeds `web/dist/` into the binary as compressed bytes
- Axum routes: `/api/*` hits existing handlers, `/*` serves static files with SPA fallback to `index.html`
- Dev mode: Vite dev server on `:5173` proxies `/api` to Axum on `:3000`
- Production: `genesis serve` serves both API and UI on a single port
- The UI is gated behind a Cargo feature flag (`embed-ui`) — building without it produces an API-only binary

## Backend Changes

### Route prefix migration

Existing gateway routes (currently at root: `/health`, `/chat`, `/sessions`, etc.) must be nested under `/api/` so the SPA catch-all doesn't shadow them. This is a one-time refactor in `build_router()`:

```rust
// Before: Router::new().route("/health", get(health_handler))
// After:  Router::new().nest("/api", api_routes).fallback(static_handler)
```

The following routes **stay at root** (not nested under `/api/`):
- Platform webhooks (`/telegram/webhook`, `/discord/interactions`, `/slack/events`, `/whatsapp/webhook`, `/homeassistant/webhook`, `/signal/webhook`, `/signal/poll`) — not consumed by the UI, have their own auth
- `/health` and `/health/mcp` — external monitoring tools expect these at root
- `/.well-known/agent.json` — must be at root per RFC 8615 (A2A discovery)
- `/v1/chat/completions` and `/v1/models` — OpenAI-compatible endpoints, external tooling (LiteLLM, OpenWebUI) expects `/v1/` prefix
- `/metrics` — Prometheus scrape configs expect this at root

### New dependencies

```toml
# workspace Cargo.toml [workspace.dependencies]
rust-embed = { version = "8", features = ["compression"] }
mime_guess = "2"

# crates/genesis-gateway/Cargo.toml [dependencies]
rust-embed = { workspace = true, optional = true }
mime_guess = { workspace = true, optional = true }

# crates/genesis-gateway/Cargo.toml [features]
embed-ui = ["dep:rust-embed", "dep:mime_guess"]
```

### Metrics JSON endpoint

`GET /api/metrics` returns Prometheus text format. Add `GET /api/metrics/json` returning a JSON summary (uptime, request count, token totals, error count, histogram percentiles) so the dashboard can consume it without parsing Prometheus exposition format.

## Page Structure & Routing

| Route | View | Description |
|-------|------|-------------|
| `/` | Dashboard | Health KPIs, token usage chart, platform breakdown, recent sessions |
| `/sessions` | Session List | Search, filter by platform/tag, paginate |
| `/sessions/:id` | Session Detail | Message history, tool call blocks, tags, fork/export/delete |
| `/skills` | Skills Manager | CRUD, usage stats, tag filtering |
| `/memories` | Memory Explorer | Full-text search, browse, delete, embedding status |
| `/schedules` | Schedule Manager | List, create, enable/disable, cron expression preview |
| `/tools` | Tool Registry | Builtin + MCP tools, usage analytics |
| `/analytics` | Analytics | Token/session charts over time, platform breakdown, tool usage |
| `/audit` | Audit Log | Filterable by session, time range, action type |
| `/settings` | Settings | Config viewer, MCP server status, pairing management |

### Layout

- **Sidebar:** Collapsible (Shadcn Sidebar component), nav links, Eve branding, health status indicator. Collapses to icons on narrow screens, sheet on mobile.
- **Top bar:** Current page title, `Cmd+K` command palette (Shadcn Command), connection status dot.
- **Main content:** Page-specific content with consistent padding and max-width.

### TanStack Router Details

- File-based route definitions in `web/src/routes/`
- Type-safe search params for filters: `/sessions?search=foo&platform=telegram&page=2`
- Loader functions prefetch data before route renders (no loading spinners on nav)
- `pendingComponent` for slow loads

## Data Layer

### API Client

```
web/src/lib/api/
├── client.ts          # Shared fetch wrapper (base URL, auth header, error handling)
├── queries/
│   ├── health.ts      # useHealth()         → GET /api/health          (poll 5s)
│   ├── sessions.ts    # useSessions()       → GET /api/sessions        (poll 30s)
│   │                  # useSession(id)      → GET /api/sessions/:id
│   │                  # useMessages(id)     → GET /api/sessions/:id/messages
│   ├── skills.ts      # useSkills()         → GET /api/skills
│   ├── memories.ts    # useMemories()       → GET /api/memories
│   ├── schedules.ts   # useSchedules()      → GET /api/schedules
│   ├── analytics.ts   # useInsights(days)   → GET /api/insights
│   │                  # useUsage()          → GET /api/usage
│   │                  # useToolAnalytics()  → GET /api/analytics/tools
│   ├── audit.ts       # useAuditLog()       → GET /api/audit
│   └── metrics.ts     # useMetricsJson()    → GET /api/metrics/json  (poll 10s) [NEW ENDPOINT]
└── mutations/
    ├── sessions.ts    # useDeleteSession    → DELETE /api/sessions/:id
    │                  # useForkSession      → POST /api/sessions/:id/fork
    │                  # useUpdateTitle      → PATCH /api/sessions/:id/title
    │                  # useAddTag           → POST /api/sessions/:id/tags/:tag
    │                  # useRemoveTag        → DELETE /api/sessions/:id/tags/:tag
    ├── skills.ts      # useCreateSkill      → POST /api/skills
    │                  # useDeleteSkill      → DELETE /api/skills/:name
    ├── schedules.ts   # useCreateSchedule   → POST /api/schedules
    │                  # useDeleteSchedule   → DELETE /api/schedules/:id
    │                  # useToggleSchedule   → PATCH /api/schedules/:id/enabled
    └── memories.ts    # useDeleteMemory     → DELETE /api/memories/:id
                       # useEmbedMemory      → POST /api/memories/:id/embed  (single)
                       # useEmbedAll         → POST /api/memories/embed      (batch)
```

### Polling Strategy

- Most views use TanStack Query polling at configurable intervals
- All mutations auto-invalidate related queries (e.g., delete skill → skills list refetches)
- No SSE/live streaming in v1 — session detail shows static message history via `GET /api/sessions/:id/messages`. Live session observation (watching an in-progress agent loop) is a v2 feature requiring a new backend endpoint

### Auth

- Login form stores API key in `localStorage`
- Sent as `Authorization: Bearer <key>` on every request
- If no API key is configured on the server, everything works without auth
- No multi-user auth, no RBAC — single operator model

### Error Handling

- TanStack Query retries (3x with exponential backoff)
- Toast notifications for mutation failures (Shadcn Sonner)
- Connection lost banner when health poll fails

## Key Views — Detail

### Dashboard (`/`)

- **KPI row (4 cards):** Status (healthy/degraded + uptime), Total sessions (from `GET /api/health` → `total_sessions`), Tokens 24h (in/out split, from `GET /api/insights?days=1`), Active tools (builtin + MCP count)
- **Charts row:** Token usage bar chart (7d) + Platform mix horizontal bars
- **Recent sessions table:** ID, title, platform, token count, relative time. Clickable rows → session detail.
- Health polls every 5s, metrics every 10s.

### Session Detail (`/sessions/:id`)

- **Header:** Title, session ID, platform, turn count, token count, estimated cost. Actions: Fork, Export, Delete.
- **Tags:** Inline tag badges with add/remove.
- **Message thread:** User messages (cyan left border), Eve responses (purple left border), tool call blocks (collapsible, showing tool name, duration, truncated params/result).
- **Subagents panel:** If session has subagents, show as nested expandable cards.

### Skills Manager (`/skills`)

- **Header:** Search input + tag filter dropdown + "New Skill" button.
- **Table:** Name, tags, usage count (30d), last used, actions menu (edit, delete, view usage).
- **Detail/edit panel:** Slide-over or modal with skill content editor, tag management, file attachments.

### Memories, Schedules, Audit

Follow the same pattern: search/filter bar → data table (TanStack Table) → detail view. Schedules additionally show a cron expression preview ("Next run: in 3h 22m") and enable/disable toggle.

## Visual Design

**Aesthetic:** Terminal/hacker — dark backgrounds (#0a0a0a), monospace accents, high data density, subtle borders (#262626), cyan/purple/green accent colors. Inspired by Grafana dark mode and Vercel's dashboard.

**Shadcn theme customization:**
- Override CSS variables for dark terminal palette
- Monospace font (JetBrains Mono or similar) for data, sans-serif (Inter) for UI chrome
- Dense spacing — less padding than default Shadcn
- Accent colors: cyan (#0891b2) for primary actions, purple (#a855f7) for Eve, green (#22c55e) for success, amber (#eab308) for warnings

## Build Pipeline

### Development

```bash
# Terminal 1: Rust backend
cargo run -- serve

# Terminal 2: Frontend with API proxy
cd web && npm run dev
```

`vite.config.ts` proxies `/api/*` and `/chat/ws` to `http://localhost:3000`.

### Production

```bash
# Build frontend
cd web && npm ci && npm run build

# Build binary with embedded assets
cargo build --release
```

### Build Orchestration (justfile)

```just
build-web:
    cd web && npm ci && npm run build

build: build-web
    cargo build --release

dev:
    parallel "cargo run -- serve" "cd web && npm run dev"
```

### Embedding (genesis-gateway)

Gated behind the `embed-ui` Cargo feature. When the feature is disabled, no static file handler is registered and the binary behaves as before (API-only).

```rust
#[cfg(feature = "embed-ui")]
mod web_assets {
    use rust_embed::Embed;

    #[derive(Embed)]
    #[folder = "web/dist/"]
    pub struct WebAssets;
}

#[cfg(feature = "embed-ui")]
async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    match web_assets::WebAssets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => {
            // SPA fallback — serve index.html for client-side routing
            match web_assets::WebAssets::get("index.html") {
                Some(index) => {
                    ([(header::CONTENT_TYPE, "text/html")], index.data).into_response()
                }
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    }
}
```

- Feature-gated: `cargo build --features embed-ui` embeds the UI; without the flag, API-only binary
- `web/dist/` must exist when building with `embed-ui` (build.rs can verify and error clearly)
- Hashed filenames for cache busting with long-lived cache headers
- New deps: `rust-embed` (v8, compression) + `mime_guess` (v2) added to workspace

## Project Structure

```
web/
├── package.json
├── tsconfig.json
├── vite.config.ts
├── tailwind.config.ts
├── index.html
├── public/
│   └── favicon.svg
├── src/
│   ├── main.tsx                    # Entry point, QueryClient, RouterProvider
│   ├── router.tsx                  # TanStack Router setup
│   ├── routes/
│   │   ├── __root.tsx              # Root layout (sidebar + topbar)
│   │   ├── index.tsx               # Dashboard
│   │   ├── sessions/
│   │   │   ├── index.tsx           # Session list
│   │   │   └── $id.tsx             # Session detail
│   │   ├── skills.tsx
│   │   ├── memories.tsx
│   │   ├── schedules.tsx
│   │   ├── tools.tsx
│   │   ├── analytics.tsx
│   │   ├── audit.tsx
│   │   └── settings.tsx
│   ├── components/
│   │   ├── ui/                     # Shadcn components (copied in)
│   │   ├── layout/
│   │   │   ├── sidebar.tsx
│   │   │   ├── topbar.tsx
│   │   │   └── command-palette.tsx
│   │   ├── dashboard/
│   │   │   ├── kpi-card.tsx
│   │   │   ├── token-chart.tsx
│   │   │   └── platform-breakdown.tsx
│   │   ├── sessions/
│   │   │   ├── session-table.tsx
│   │   │   ├── message-thread.tsx
│   │   │   └── tool-call-block.tsx
│   │   └── shared/
│   │       ├── data-table.tsx      # Generic TanStack Table wrapper
│   │       ├── connection-banner.tsx
│   │       └── empty-state.tsx
│   ├── lib/
│   │   ├── api/
│   │   │   ├── client.ts
│   │   │   ├── queries/
│   │   │   └── mutations/
│   │   └── utils.ts
│   └── styles/
│       └── globals.css             # Tailwind + Shadcn theme overrides
└── components.json                 # Shadcn config
```

## What's NOT in v1

- Web-based chat with Eve (future v2)
- Live session observation (watching an in-progress agent loop via SSE — requires new backend endpoint)
- Multi-user auth / RBAC
- MCP server management (add/remove/configure)
- Skill content editor with syntax highlighting
- Mobile-optimized layouts (functional but not polished)
- Internationalization

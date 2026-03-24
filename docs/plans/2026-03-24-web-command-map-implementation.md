# Web Command Map Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current passive monitor radar with an Eve-centered command map that supports layered topology, node search, inspection, pinning, and operational dialogs.

**Architecture:** Build a command-map adapter layer that projects existing backend data into a stable node model, then render that model through a dedicated monitor shell with canvas, inspector, toolbar, and dialogs. Start with polling over current REST endpoints and local layout persistence so the UI is useful before a richer live topology API exists.

**Tech Stack:** React 19, TypeScript, Vite, TanStack Router, TanStack Query, Tailwind 4, existing command palette components, plus a graph-canvas library such as `@xyflow/react`.

---

### Task 1: Add Frontend Test Harness For Command Map Work

**Files:**
- Modify: `web/package.json`
- Modify: `web/vite.config.ts`
- Create: `web/src/test/setup.ts`
- Create: `web/src/lib/command-map/selectors.test.ts`

**Step 1: Write the failing test**

Create `web/src/lib/command-map/selectors.test.ts` with a minimal projection test:

```ts
import { describe, expect, it } from 'vitest'
import { buildCommandMapModel } from './selectors'

describe('buildCommandMapModel', () => {
  it('creates Eve plus derived execution and trigger nodes', () => {
    const model = buildCommandMapModel({
      health: { status: 'ok', version: '1.0.0', uptime_seconds: 60, model: 'gpt', mcp_servers: 1, active_schedules: 1, total_sessions: 2, total_tools: 3 },
      sessions: [{ id: 's1', title: 'Alpha', platform: 'api', total_input_tokens: 10, total_output_tokens: 5, parent_session_id: null, created_at: '2026-03-24T00:00:00Z', updated_at: '2026-03-24T00:00:00Z' }],
      schedules: [{ id: 'nightly', cron_expression: '0 0 * * *', destination: 'api', prompt: 'Run nightly', enabled: true, created_at: '2026-03-24T00:00:00Z', last_run_at: null }],
      audit: [],
      insights: null,
    })

    expect(model.nodes.map(node => node.id)).toContain('eve')
    expect(model.nodes.some(node => node.kind === 'thread')).toBe(true)
    expect(model.nodes.some(node => node.kind === 'trigger')).toBe(true)
  })
})
```

**Step 2: Run test to verify it fails**

Run: `cd web && npm test -- selectors.test.ts`

Expected: FAIL because test tooling and `buildCommandMapModel` do not exist yet.

**Step 3: Add minimal test tooling**

- add `vitest`, `jsdom`, `@testing-library/react`, and `@testing-library/jest-dom`
- add `test` script to `web/package.json`
- wire test setup in `web/vite.config.ts`
- create `web/src/test/setup.ts`

**Step 4: Run test to verify tooling works and the selector test still fails for the right reason**

Run: `cd web && npm test -- selectors.test.ts`

Expected: FAIL with module/function missing, not with test-runner setup errors.

**Step 5: Commit**

```bash
git add web/package.json web/vite.config.ts web/src/test/setup.ts web/src/lib/command-map/selectors.test.ts
git commit -m "test(web): add command map frontend test harness"
```

### Task 2: Build Command Map Data Model And Projection Layer

**Files:**
- Create: `web/src/lib/command-map/types.ts`
- Create: `web/src/lib/command-map/selectors.ts`
- Create: `web/src/lib/command-map/layout.ts`
- Modify: `web/src/lib/api/types.ts`
- Test: `web/src/lib/command-map/selectors.test.ts`

**Step 1: Expand the failing tests**

Add tests covering:

- Eve core node creation
- execution node derivation from sessions
- trigger node derivation from schedules
- alert node derivation from failed/degraded audit entries
- stable ring/layer assignment

**Step 2: Run tests to verify they fail**

Run: `cd web && npm test -- selectors.test.ts`

Expected: FAIL because selectors and types are still incomplete.

**Step 3: Write minimal implementation**

Create `types.ts` with:

- `CommandMapNodeKind`
- `CommandMapNode`
- `CommandMapEdge`
- `CommandMapModel`
- `CommandMapProjectionInput`

Create `selectors.ts` with:

- `buildCommandMapModel(input)`
- `buildEveNode(...)`
- `buildSessionNodes(...)`
- `buildScheduleNodes(...)`
- `buildSystemNodes(...)`
- `buildAlertNodes(...)`

Create `layout.ts` with:

- ring constants
- default auto-placement helpers
- pin-aware position merge helpers

**Step 4: Run tests to verify they pass**

Run: `cd web && npm test -- selectors.test.ts`

Expected: PASS.

**Step 5: Commit**

```bash
git add web/src/lib/command-map/types.ts web/src/lib/command-map/selectors.ts web/src/lib/command-map/layout.ts web/src/lib/command-map/selectors.test.ts web/src/lib/api/types.ts
git commit -m "feat(web): add command map projection model"
```

### Task 3: Replace The Passive Monitor Canvas With Command Map Shell

**Files:**
- Modify: `web/src/routes/monitor.lazy.tsx`
- Replace/Modify: `web/src/components/monitor/agent-canvas.tsx`
- Create: `web/src/components/monitor/command-map.tsx`
- Create: `web/src/components/monitor/command-map-toolbar.tsx`
- Create: `web/src/components/monitor/command-map-inspector.tsx`
- Create: `web/src/components/monitor/use-command-map-state.ts`

**Step 1: Write the failing UI test**

Add a test asserting that monitor renders:

- Eve node
- layer toggles
- inspector placeholder after selection

Run: `cd web && npm test -- command-map`

Expected: FAIL because the new monitor shell does not exist.

**Step 2: Implement the command-map shell**

Create:

- `command-map.tsx` for canvas composition
- `command-map-toolbar.tsx` for layer toggles, reset, focus, search trigger
- `command-map-inspector.tsx` for selected-node details
- `use-command-map-state.ts` for selection, filters, declutter, and focus state

Update `monitor.lazy.tsx` to:

- fetch health, sessions, schedules, analytics, and audit
- project them through `buildCommandMapModel`
- render the command map shell instead of the old passive radar + stats sidebar split

Keep the old `agent-canvas.tsx` only if it remains useful as a temporary visual helper; otherwise replace it entirely.

**Step 3: Run tests and build**

Run:

- `cd web && npm test -- command-map`
- `cd web && npm run build`

Expected: PASS.

**Step 4: Commit**

```bash
git add web/src/routes/monitor.lazy.tsx web/src/components/monitor/agent-canvas.tsx web/src/components/monitor/command-map.tsx web/src/components/monitor/command-map-toolbar.tsx web/src/components/monitor/command-map-inspector.tsx web/src/components/monitor/use-command-map-state.ts
git commit -m "feat(web): add monitor command map shell"
```

### Task 4: Add Graph Canvas, Hybrid Layout, And Manual Pinning

**Files:**
- Modify: `web/package.json`
- Modify: `web/src/components/monitor/command-map.tsx`
- Create: `web/src/components/monitor/node-renderers.tsx`
- Create: `web/src/components/monitor/use-node-layout.ts`
- Create: `web/src/lib/command-map/storage.ts`
- Test: `web/src/lib/command-map/selectors.test.ts`

**Step 1: Write the failing tests**

Add tests for:

- pinned positions override auto-layout
- unpinned nodes reflow while pinned nodes stay stable
- reset layout clears stored positions

**Step 2: Run tests to verify they fail**

Run: `cd web && npm test -- selectors.test.ts`

Expected: FAIL on pin/layout behavior.

**Step 3: Implement minimal layout persistence**

- add graph-canvas dependency such as `@xyflow/react`
- create node renderers for Eve, thread, trigger, system, alert, and recipe
- add `use-node-layout.ts` for auto-layout + pinned override merge
- persist pinned positions in `storage.ts` via `localStorage`

**Step 4: Run tests and build**

Run:

- `cd web && npm test -- selectors.test.ts`
- `cd web && npm run build`

Expected: PASS.

**Step 5: Commit**

```bash
git add web/package.json web/src/components/monitor/command-map.tsx web/src/components/monitor/node-renderers.tsx web/src/components/monitor/use-node-layout.ts web/src/lib/command-map/storage.ts web/src/lib/command-map/selectors.test.ts
git commit -m "feat(web): add hybrid layout and pinning for command map"
```

### Task 5: Integrate Search And Jump-To-Node Navigation

**Files:**
- Modify: `web/src/components/layout/command-palette.tsx`
- Modify: `web/src/components/monitor/use-command-map-state.ts`
- Create: `web/src/lib/command-map/search.ts`
- Test: `web/src/lib/command-map/search.test.ts`

**Step 1: Write the failing tests**

Add tests for:

- indexing nodes by type, status, and title
- jumping to a node returns the correct focus target
- filters can restrict search by layer/type

**Step 2: Run test to verify it fails**

Run: `cd web && npm test -- search.test.ts`

Expected: FAIL because search index helpers do not exist.

**Step 3: Implement minimal search integration**

- create `search.ts` to build search items from the command-map model
- feed those items into the existing command palette
- on selection, center viewport, pulse-highlight node, and open inspector

**Step 4: Run tests and build**

Run:

- `cd web && npm test -- search.test.ts`
- `cd web && npm run build`

Expected: PASS.

**Step 5: Commit**

```bash
git add web/src/components/layout/command-palette.tsx web/src/components/monitor/use-command-map-state.ts web/src/lib/command-map/search.ts web/src/lib/command-map/search.test.ts
git commit -m "feat(web): add command map node search and jump"
```

### Task 6: Add Inspector Actions, Dialogs, And Mobile Sheet Behavior

**Files:**
- Modify: `web/src/components/monitor/command-map-inspector.tsx`
- Create: `web/src/components/monitor/run-recipe-dialog.tsx`
- Create: `web/src/components/monitor/edit-trigger-dialog.tsx`
- Create: `web/src/components/monitor/thread-details-dialog.tsx`
- Create: `web/src/components/monitor/event-log-drawer.tsx`
- Modify: `web/src/hooks/use-mobile.ts`

**Step 1: Write the failing UI tests**

Add tests that assert:

- selecting a node opens the inspector
- running a recipe opens a dialog
- editing a trigger opens a dialog
- mobile selection uses a sheet/drawer instead of relying on hover

**Step 2: Run tests to verify they fail**

Run: `cd web && npm test -- command-map`

Expected: FAIL on missing dialogs/actions.

**Step 3: Implement minimal overlay system**

- wire action buttons from inspector
- use existing dialog/sheet primitives
- keep one primary overlay active at a time
- use drawers for logs and mobile detail where appropriate

**Step 4: Run tests and build**

Run:

- `cd web && npm test -- command-map`
- `cd web && npm run build`

Expected: PASS.

**Step 5: Commit**

```bash
git add web/src/components/monitor/command-map-inspector.tsx web/src/components/monitor/run-recipe-dialog.tsx web/src/components/monitor/edit-trigger-dialog.tsx web/src/components/monitor/thread-details-dialog.tsx web/src/components/monitor/event-log-drawer.tsx web/src/hooks/use-mobile.ts
git commit -m "feat(web): add command map dialogs and inspector actions"
```

### Task 7: Final Verification And Cleanup

**Files:**
- Modify: `web/src/routes/monitor.lazy.tsx`
- Modify: `web/src/components/monitor/*`
- Modify: `docs/plans/2026-03-24-web-command-map-design.md`

**Step 1: Run full verification**

Run:

- `cargo test --workspace --quiet`
- `cd web && npm test`
- `cd web && npm run build`

Expected: PASS.

**Step 2: Manual verification**

Run: `cd web && npm run dev`

Check:

- Eve centered by default
- layer toggles work
- search jumps to nodes
- pinned nodes persist
- inspector/dialog flow works on desktop and mobile widths
- canvas remains readable under dense data

**Step 3: Make only necessary cleanup**

- remove dead radar-only code if unused
- tighten naming
- avoid theme-heavy styling changes

**Step 4: Commit**

```bash
git add web/src/routes/monitor.lazy.tsx web/src/components/monitor docs/plans/2026-03-24-web-command-map-design.md
git commit -m "refactor(web): finalize monitor command map experience"
```

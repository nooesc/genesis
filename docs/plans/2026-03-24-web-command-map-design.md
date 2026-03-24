# Web Command Map Design

**Date:** 2026-03-24
**Status:** Approved for planning
**Primary Surface:** `/monitor`

## Goal

Turn the existing web monitor into a command-and-observe center for Genesis.

The center of the experience is **Eve**, not a generic dashboard and not a workflow editor. The canvas should answer three questions quickly:

1. What is Eve doing now?
2. What can the operator inspect or intervene in?
3. Which reusable recipes and triggers are ready to run?

## Product Position

This is not an "AI-themed OS" in v1. The product should feel like serious operational software with room for later theming.

The right product shape is a **Command Map**:

- `primary`: observe Eve and live execution state
- `secondary`: inspect nodes and intervene
- `tertiary`: launch saved recipes and manage triggers

## Core Constraint

The current web app already exposes summaries for health, sessions, schedules, analytics, audit, and session detail. It does **not** yet expose a first-class live execution graph for threads, subagents, and tool activity.

That means v1 should be an **operational map**, not a freeform whiteboard or node editor. The canvas should synthesize topology from real backend data, then progressively gain richer live graph state over time.

## Primary Mental Model

Eve is the stable center of the system.

Everything on the map should read as one of:

- feeding Eve
- being spawned by Eve
- scheduled for Eve
- observed by Eve
- supporting Eve

The default view should feel like concentric operational layers around Eve.

## Node Taxonomy

### Core Node

`Eve`

- fixed at center in default layout
- shows health, model/runtime identity, active load, recent event pulse
- actions: inspect core, open command dialog, center/reset map

### Execution Nodes

`Thread`, `Subagent`, `Run`

- closest to Eve
- most dynamic and most animated
- show status, age, recent activity, and parent/child relationships
- actions: inspect, open full detail dialog, cancel if supported

### Recipe Nodes

Reusable skill/prompt bundles for frequent tasks.

- not editable as flowcharts on the canvas
- show name, trigger type, last run, status summary
- actions: run now, inspect recipe, pin/unpin, open trigger dialog

### Trigger Nodes

`Cron`, `Webhook`, `Manual`, future event-driven sources

- connected to recipe nodes
- show enabled state, next fire time, failure status
- actions: enable/disable, edit trigger, open history

### System Nodes

`MCP`, `Platform`, `Model`, `Memory`, `Storage`, future infrastructure nodes

- live farther from center unless degraded
- show connected/degraded/offline status and a small summary
- actions: inspect, jump to related route, open diagnostics

### Alert/Event Nodes

Temporary or collapsible markers for degraded state and failures.

- surface urgency without becoming permanent clutter
- actions: open logs, acknowledge, filter related nodes

## Layout and Layering

The map should behave like a controlled operations surface.

### Default Placement

- `Eve`: anchored center
- `Execution`: inner ring
- `Recipes` and `Triggers`: stable middle ring
- `Systems`: outer ring
- `Alerts`: may break ring placement and move closer to center

### Hybrid Placement

The layout should be **hybrid**:

- default is system-managed placement
- operators can pin important nodes manually
- unpinned nodes continue to auto-flow around pinned anchors
- provide `reset layout`, `repack`, and `focus layer`

### Layer Controls

- toggle `Execution`, `Recipes`, `Triggers`, `Systems`, `Alerts`
- `declutter` mode hides low-priority nodes and edges
- `focus` mode dims everything except the selected node neighborhood

## Search and Navigation

The existing command palette should evolve into a node navigator.

Search must support:

- sessions
- recipes
- schedules/triggers
- MCP services
- platforms
- alerts

Selecting a result should:

- center the viewport
- pulse-highlight the node
- open the inspector

Filters should include:

- type
- status
- recency
- pinned
- has errors

## Interaction Model

The canvas should support three depths of interaction.

### Hover

- lightweight status preview
- relationship highlight
- quick metrics

### Select

- persistent inspector on desktop
- bottom sheet on mobile
- node context, metrics, recent events, related nodes, available actions

### Act

Focused dialogs or drawers for commands, edits, and deep inspection.

## Dialogs and Drawers

Dialogs should carry dense operational detail so the map stays readable.

Required v1 overlays:

- `Run Recipe`
- `Edit Trigger`
- `Thread Details`
- `Logs / Event Stream`
- `Core Command`
- destructive confirmation dialog

Design rule:

- status belongs on the map or in the inspector
- forms and heavy detail belong in dialogs/drawers

## V1 Scope

### In Scope

- replace the passive radar visualization with a real node-based command map
- keep Eve at the center
- derive nodes from existing health, session, schedule, audit, and analytics data
- support hybrid auto-layout with manual pinning
- add node search and jump-to-node behavior
- add inspector plus command/action dialogs
- keep the current ops-oriented tone and avoid theme-heavy work

### Explicitly Out of Scope

- freeform workflow drawing
- whiteboard behavior
- user-authored edge editing
- heavy "AI OS" visual styling
- full live execution graph from a new backend stream

## Technical Direction

The current stack is already suitable for this:

- React 19
- TanStack Router
- TanStack Query
- Tailwind 4
- existing command palette and route shell

The recommended canvas direction is a graph canvas with viewport control and node rendering, not a whiteboard SDK. That supports stable layout, inspection, search, pinning, and overlays without implying arbitrary graph editing.

## Proposed Information Architecture

The monitor route should become the main operations surface.

### Keep

- top system bar
- command palette
- route shell

### Evolve

- replace `AgentCanvas` with a composable command-map canvas
- upgrade the monitor sidebar into a real inspector
- integrate node search with the command palette

### Reuse Existing Data

- health
- sessions
- schedules
- audit
- analytics
- session detail

## Open Follow-Up for Implementation

The biggest product/architecture gap is the absence of a dedicated live topology API for threads, subagents, and tool activity. The implementation should therefore:

- start with projections over existing data
- isolate the projection logic behind a command-map adapter layer
- make it easy to swap to a richer backend feed later

## Success Criteria

The first implementation is successful if:

- the operator can open `/monitor` and immediately understand whether Eve is healthy
- active and important nodes are discoverable without route-hopping
- search can reliably locate and focus any important node
- the canvas stays readable under both light and heavy load
- dialogs enable action without turning the canvas into a form-heavy dashboard

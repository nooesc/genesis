# Welcome Screen (ASCII Girl + Session Metadata) Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan.

**Goal:** Rebuild the TUI welcome screen as a split hero panel with a terminal-native Eve portrait and rich session metadata (model/backend/tools/skills/cwd) while preserving existing key hints.

**Architecture:** Keep rendering inside `WelcomeWidget::render` and compute all startup data before entering the event loop. `run_tui` will enrich `WelcomeInfo` once (single async lookup where needed) and pass it into the existing welcome state. No runtime spawning or per-frame process calls.

**Tech Stack:** Rust + ratatui + crossterm + existing Genesis crates (`genesis-ui`, `genesis-core`, `genesis-config`, `genesis-storage`, etc.).

---

## Design Decision

We evaluated three art directions for the startup screen:

1. `Shaded portrait` — classic ASCII face/bust with density-based shading
2. `Typographic silhouette` — a figure built from repeated token clusters
3. `Hybrid portrait` — portrait outline with selective typographic fill

The selected direction is `hybrid portrait`, but with a bias toward legibility over cleverness. The terminal startup screen should read as a character first and a brand treatment second. That means the portrait silhouette and face need to remain recognizable at a glance, while any typographic fill should be limited to larger massed regions like hair or clothing.

## Responsive Layout

The welcome screen should degrade intentionally across three width bands:

1. `>= 100 cols` — split layout:
art on the left, title + metadata + key hints on the right, using the full hybrid portrait.
2. `70-99 cols` — centered compact layout:
use a simplified contour portrait above the title and metadata.
3. `< 70 cols` — text-only layout:
skip the portrait entirely and render only the textual welcome.

This avoids bad wrapping and keeps the startup screen stable across typical terminal sizes.

## Task 1: Extend welcome metadata model

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

- Add fields to `WelcomeInfo`:
  - `backend: String`
  - `session_id: String`
  - `tool_count_builtin: usize`
  - `tool_count_mcp: usize`
  - `skill_count: usize`

- Keep `cwd` and `model`/`version` and preserve backwards-compatible constructors (or update all call sites in the same patch).

- Update tests that instantiate `WelcomeInfo` to include new fields.

## Task 2: Add startup metadata gathering in TUI entrypoint

**Files:**
- Modify: `crates/genesis-tui/src/lib.rs`

- Before constructing `WelcomeWidget`, gather one-time info:
  - backend and model from `config.provider`
  - cwd from `std::env::current_dir()`
  - tool counts via `service.tool_counts().await`
  - skill count via `SkillStore::new(&config.storage.database_path).list_all().map_or(0, |s| s.len())` with graceful fallback

- Pass new fields into `WelcomeWidget::new(WelcomeInfo { ... })`.

- Update `Cargo.toml` in `crates/genesis-tui` to include `genesis-storage` if skill count is computed directly.

## Task 3: Render split welcome layout with ASCII portrait

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

- Replace the current placeholder art with two static art blocks:
  - `ASCII_GIRL_WIDE` for the hybrid portrait
  - `ASCII_GIRL_COMPACT` for the simplified portrait

- Keep the art terminal-native:
  - plain ASCII only
  - no image loading
  - no animation
  - no half-block rendering

- Prefer contour and sparse shading over dense fill so the portrait survives terminal resizing.

- Implement adaptive layout in `WelcomeWidget::render`:
  - **Wide mode** (`width >= 100`): portrait on left, metadata/hints on right with divider spacing.
  - **Compact mode** (`70 <= width < 100`): stack compact portrait, title, metadata, and hints.
  - **Text-only mode** (`width < 70`): render title/info/hints only.

- Use `Line`/`Span` with palette constants from `genesis_ui::colors` and keep no external image deps in the welcome widget.

## Task 4: Keep key hints and existing behavior

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

- Keep existing hints:
  - Enter, /, Ctrl+T, Ctrl+C, Ctrl+D
- Keep start-any-key behavior unchanged in `App::handle_key` (handled elsewhere).
- Keep the current richer metadata content:
  - session
  - model
  - backend
  - cwd
  - tools
  - skills

## Task 5: Validation and docs

**Files:**
- Run the existing welcome widget tests and update/extend where necessary for new fields and new rendering helpers.
- Confirm no regressions for:
  - zero-area render path
  - split layout render path
  - compact portrait render path
  - text-only render path
- Update plan doc if any edge case decisions change during implementation.

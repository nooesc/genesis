# Welcome Screen (ASCII Girl + Session Metadata) Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan.

**Goal:** Rebuild the TUI welcome screen as a split hero panel with an ASCII-girl icon and rich session metadata (model/backend/tools/skills/cwd) while preserving existing key hints.

**Architecture:** Keep rendering inside `WelcomeWidget::render` and compute all startup data before entering the event loop. `run_tui` will enrich `WelcomeInfo` once (single async lookup where needed) and pass it into the existing welcome state. No runtime spawning or per-frame process calls.

**Tech Stack:** Rust + ratatui + crossterm + existing Genesis crates (`genesis-ui`, `genesis-core`, `genesis-config`, `genesis-storage`, etc.).

---

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

## Task 3: Render split welcome layout with ASCII girl

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

- Add a static ASCII-girl art block (constant `&[&str]`), using plain glyphs only.

- Implement adaptive layout in `WelcomeWidget::render`:
  - **Wide mode** (`width >= 110`): logo on left, metadata/hints on right with divider spacing.
  - **Compact mode** (`width < 110`): stack logo then centered title/info/hints.

- Preserve existing center fallback for very small widths.

- Use `Line`/`Span` with palette constants from `genesis_ui::colors` and keep no external image deps in the welcome widget.

## Task 4: Keep key hints and existing behavior

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

- Keep existing hints:
  - Enter, /, Ctrl+T, Ctrl+C, Ctrl+D
- Keep start-any-key behavior unchanged in `App::handle_key` (handled elsewhere).

## Task 5: Validation and docs

**Files:**
- Run the existing welcome widget tests and update/extend where necessary for new fields and new rendering helpers.
- Confirm no regressions for zero-area and short-width render paths.
- Update plan doc if any edge case decisions change during implementation.


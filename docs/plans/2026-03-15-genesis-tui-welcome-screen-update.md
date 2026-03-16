# Genesis TUI Welcome Screen Update Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the placeholder welcome art with a terminal-native Eve portrait that degrades cleanly across wide, medium, and narrow terminal widths while preserving the richer startup metadata.

**Architecture:** Keep all rendering logic inside `WelcomeWidget` and continue constructing `WelcomeInfo` once in `run_tui`. The widget should choose between three explicit layout modes based on terminal width: split portrait, compact portrait, and text-only. No animation, image decoding, or runtime asset loading should be introduced.

**Tech Stack:** Rust, ratatui, crossterm, existing `genesis-tui` widget tests

---

### Task 1: Add failing tests for responsive welcome layouts

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing tests**

Add tests that exercise:
- wide render path using the split portrait
- medium-width render path using the compact portrait
- narrow render path using the text-only fallback

Use direct `Buffer::empty(Rect::new(...))` rendering and assert that:
- the wide render includes visible portrait glyphs
- the compact render includes visible portrait glyphs
- the narrow render omits portrait glyphs but still includes the title

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: FAIL because the current widget only has one portrait variant and no explicit text-only threshold.

**Step 3: Write minimal implementation**

Introduce explicit width thresholds and separate art constants so each render mode is selected intentionally.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "test: cover responsive welcome screen modes"
```

### Task 2: Replace placeholder art with a wide hybrid portrait

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add a test that renders the wide layout and asserts the portrait uses specific stable glyph sequences from the new wide portrait instead of the current simplistic placeholder art.

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui wide_welcome -- --nocapture`
Expected: FAIL because the old art does not match the intended portrait signature.

**Step 3: Write minimal implementation**

Replace the current placeholder ASCII constant with a hand-tuned wide portrait that:
- is approximately 24-32 columns wide
- reads as a bust/character silhouette
- uses contour plus sparse shading
- keeps any typographic fill limited and readable

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui wide_welcome -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "feat: add wide Eve portrait to welcome screen"
```

### Task 3: Add a compact portrait and text-only fallback

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add tests that verify:
- medium widths use the compact portrait constant
- widths below the threshold skip all portrait art

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui compact_welcome -- --nocapture`
Expected: FAIL because the render path does not yet distinguish compact portrait vs text-only.

**Step 3: Write minimal implementation**

Add:
- `ASCII_GIRL_WIDE`
- `ASCII_GIRL_COMPACT`
- `WIDE_LAYOUT_MIN_WIDTH`
- `COMPACT_LAYOUT_MIN_WIDTH`

Refactor `render` and the line builders so the layout mode is explicit and deterministic.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui compact_welcome -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "feat: add compact and text-only welcome layouts"
```

### Task 4: Rebalance metadata and hint presentation around the new portrait

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add a test asserting that the wide layout still renders:
- title
- session
- model
- backend
- cwd
- tools
- skills
- keybinding hints

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_metadata -- --nocapture`
Expected: FAIL if any metadata is clipped away or omitted by the new layout logic.

**Step 3: Write minimal implementation**

Adjust spacing, clipping, and line composition so the portrait remains on the left without starving the metadata block on the right.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_metadata -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "refactor: rebalance welcome metadata layout"
```

### Task 5: Run targeted verification

**Files:**
- Verify: `crates/genesis-tui/src/widgets/welcome.rs`
- Verify: `crates/genesis-tui/src/lib.rs`
- Verify: `crates/genesis-tui/src/app.rs`

**Step 1: Run welcome-specific tests**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 2: Run full TUI test suite**

Run: `cargo test -p genesis-tui`
Expected: PASS

**Step 3: Run CLI integration compile/test path**

Run: `cargo test -p genesis-cli --no-default-features`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs crates/genesis-tui/src/lib.rs crates/genesis-tui/src/app.rs
git commit -m "test: verify welcome screen portrait update"
```

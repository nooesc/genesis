# Genesis TUI Welcome Screen Animated Image Upgrade Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current hand-authored welcome portrait with a bundled three-frame Eve animation rendered as monochrome half-block terminal art with one accent color, while preserving the metadata-rich welcome layout and text-only fallback on narrow terminals.

**Architecture:** Move portrait generation out of raw ASCII constants and onto the existing image-to-terminal path in `genesis-ui`. Bundle the three approved PNG frames under version control, build a welcome-specific renderer that produces ratatui-friendly lines or spans from those images, and let `genesis-tui` consume that renderable output in wide and compact welcome modes. Keep animation timing inside the existing TUI loop and disable it as soon as the app leaves the welcome screen.

**Tech Stack:** Rust, ratatui, crossterm, `genesis-tui`, `genesis-ui`, bundled PNG assets, existing test framework

---

### Task 1: Import and normalize the three source frames

**Files:**
- Create: `crates/genesis-ui/assets/welcome/eve_frame_01.png`
- Create: `crates/genesis-ui/assets/welcome/eve_frame_02.png`
- Create: `crates/genesis-ui/assets/welcome/eve_frame_03.png`
- Modify: `docs/plans/2026-03-15-genesis-tui-welcome-screen-update-design.md`

**Step 1: Copy the approved source images into the repo**

Use the user-approved `nano banana` images from Downloads as the import source only. Rename them to stable asset names under `crates/genesis-ui/assets/welcome/`.

**Step 2: Verify the assets are deterministic**

Check that the files exist at the committed paths and are the only runtime source for welcome art.

Run: `file crates/genesis-ui/assets/welcome/eve_frame_01.png crates/genesis-ui/assets/welcome/eve_frame_02.png crates/genesis-ui/assets/welcome/eve_frame_03.png`
Expected: three PNG files with consistent dimensions.

**Step 3: Commit**

```bash
git add crates/genesis-ui/assets/welcome/eve_frame_01.png crates/genesis-ui/assets/welcome/eve_frame_02.png crates/genesis-ui/assets/welcome/eve_frame_03.png docs/plans/2026-03-15-genesis-tui-welcome-screen-update-design.md
git commit -m "feat: add welcome animation source frames"
```

### Task 2: Add welcome-specific image rendering in `genesis-ui`

**Files:**
- Modify: `crates/genesis-ui/src/banner/frames.rs`
- Modify: `crates/genesis-ui/src/banner/mod.rs`
- Test: `crates/genesis-ui/src/banner/frames.rs`

**Step 1: Write the failing test**

Add tests for a welcome-render helper that:
- loads one bundled frame
- renders it into half-block output for a small target size
- applies a stylized monochrome palette
- preserves a sparse accent color path

Assert that:
- output is non-empty
- output dimensions are bounded by the target size
- accent usage is present but limited

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-ui welcome_frame -- --nocapture`
Expected: FAIL because no welcome-specific renderer exists yet.

**Step 3: Write minimal implementation**

Add a focused helper in `frames.rs` that:
- loads the bundled PNG
- scales/crops for welcome usage
- converts to half-block rows
- maps colors to a monochrome-plus-accent palette

Export only the minimal API needed by `genesis-tui`.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-ui welcome_frame -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-ui/src/banner/frames.rs crates/genesis-ui/src/banner/mod.rs
git commit -m "feat: add welcome half-block frame renderer"
```

### Task 3: Add welcome animation state and frame selection in `genesis-tui`

**Files:**
- Modify: `crates/genesis-tui/src/app.rs`
- Modify: `crates/genesis-tui/src/lib.rs`
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`
- Test: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add tests for welcome-state frame selection that verify:
- frame index advances over time while welcome is visible
- animation stops advancing after the welcome screen is dismissed
- the widget can render a static first frame if animation state is unavailable

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_animation -- --nocapture`
Expected: FAIL because welcome currently has no image-backed frame state.

**Step 3: Write minimal implementation**

Introduce minimal animation state:
- active frame index
- last frame tick timestamp or tick counter
- fixed low-FPS cadence

Advance frames only in the welcome state. Avoid extra background tasks or per-frame decoding.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_animation -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/app.rs crates/genesis-tui/src/lib.rs crates/genesis-tui/src/widgets/welcome.rs
git commit -m "feat: add animated welcome frame state"
```

### Task 4: Replace ASCII portrait rendering with image-backed layouts

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`
- Test: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add tests that verify:
- wide layouts render image-derived content plus metadata
- medium layouts render a smaller image above metadata
- narrow layouts remain text-only
- old hard-coded portrait glyph signatures are gone

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: FAIL because the widget still depends on ASCII portrait constants.

**Step 3: Write minimal implementation**

Refactor `WelcomeWidget` so:
- wide mode renders image left, metadata right
- compact mode renders smaller image above metadata
- narrow mode skips image rendering entirely

Delete the old `ASCII_GIRL_*` constants and any portrait-specific string layout code that is no longer needed.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "refactor: replace welcome ascii art with image rendering"
```

### Task 5: Preserve metadata layout and key hints around the new image

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`
- Test: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add assertions that wide and compact layouts still render:
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
Expected: FAIL if the new image area starves the text block or causes clipping.

**Step 3: Write minimal implementation**

Rebalance column widths and vertical spacing so the metadata remains readable alongside the rendered image. Keep text clipping conservative and preserve the existing richer welcome content.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_metadata -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "refactor: rebalance welcome metadata around image panel"
```

### Task 6: Run full verification

**Files:**
- Verify: `crates/genesis-ui/src/banner/frames.rs`
- Verify: `crates/genesis-ui/src/banner/mod.rs`
- Verify: `crates/genesis-tui/src/widgets/welcome.rs`
- Verify: `crates/genesis-tui/src/lib.rs`
- Verify: `crates/genesis-tui/src/app.rs`

**Step 1: Run image-render tests**

Run: `cargo test -p genesis-ui welcome_frame -- --nocapture`
Expected: PASS

**Step 2: Run welcome-specific TUI tests**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 3: Run full TUI suite**

Run: `cargo test -p genesis-tui`
Expected: PASS

**Step 4: Run CLI coverage path**

Run: `cargo test -p genesis-cli --no-default-features`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-ui/src/banner/frames.rs crates/genesis-ui/src/banner/mod.rs crates/genesis-tui/src/widgets/welcome.rs crates/genesis-tui/src/lib.rs crates/genesis-tui/src/app.rs
git commit -m "test: verify animated welcome image upgrade"
```

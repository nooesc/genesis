# Genesis TUI Welcome Screen Centered Dashboard Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the current rough text-only welcome screen with a clean centered dashboard layout that is readable, intentional, and stable without relying on art, images, or animation.

**Architecture:** Keep the welcome screen entirely inside `crates/genesis-tui/src/widgets/welcome.rs`. The widget should render one centered fixed-width panel, left-align all content inside that panel, and organize the startup information into clear sections: title, metadata, divider, command legend, and call to action. Existing API hooks for the removed welcome animation path should either become inert compatibility shims or be removed if nothing depends on them.

**Tech Stack:** Rust, ratatui, crossterm, existing Genesis TUI test framework

---

### Task 1: Add failing tests for the centered dashboard layout

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add tests that render the welcome widget and assert:
- the title appears
- the subtitle appears
- metadata rows render with labels and values
- the command legend appears
- no half-block art glyphs (`▀`, `▄`) are present

Also add a test that verifies the panel contents are no longer centered line-by-line but render as a left-aligned block inside the viewport.

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: FAIL because the current widget layout is still not the intended centered dashboard composition.

**Step 3: Write minimal implementation**

Adjust `WelcomeWidget` tests and helpers so they describe the new layout clearly and fail for the current composition.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "test: cover centered welcome dashboard layout"
```

### Task 2: Refactor the welcome widget into a fixed-width centered panel

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add assertions that the widget:
- uses a bounded panel width
- truncates long paths safely
- keeps the panel centered in wide terminals

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_panel -- --nocapture`
Expected: FAIL because the current rendering logic does not fully encode the centered-panel contract.

**Step 3: Write minimal implementation**

Implement a single centered panel with:
- fixed target width
- left-aligned rows inside the panel
- manual buffer drawing or equivalent deterministic rendering

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_panel -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "refactor: center welcome content in a fixed-width panel"
```

### Task 3: Improve hierarchy and section structure

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`

**Step 1: Write the failing test**

Add tests that verify:
- title and subtitle are both present
- metadata section is separated from command legend by a divider
- CTA is present and visually distinct by style or position

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_hierarchy -- --nocapture`
Expected: FAIL if hierarchy is still too flat or sections are not clearly separated.

**Step 3: Write minimal implementation**

Restructure the content into:
- title block
- metadata block
- divider
- command legend
- CTA footer

Use existing palette colors only; do not add new visual gimmicks.

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_hierarchy -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs
git commit -m "refactor: add hierarchy to welcome dashboard"
```

### Task 4: Remove active welcome animation behavior

**Files:**
- Modify: `crates/genesis-tui/src/widgets/welcome.rs`
- Modify: `crates/genesis-tui/src/lib.rs`
- Modify: `crates/genesis-tui/src/app.rs`

**Step 1: Write the failing test**

Add or update tests to verify:
- welcome widget reports no active animation
- ticking the welcome widget is a no-op
- the render loop does not rely on welcome animation to schedule redraws

**Step 2: Run test to verify it fails**

Run: `cargo test -p genesis-tui welcome_animation -- --nocapture`
Expected: FAIL if any active animation assumptions remain.

**Step 3: Write minimal implementation**

Make the welcome screen fully static:
- remove active animation state or leave only inert compatibility hooks
- ensure the event loop does not schedule redraws for welcome animation

**Step 4: Run test to verify it passes**

Run: `cargo test -p genesis-tui welcome_animation -- --nocapture`
Expected: PASS

**Step 5: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs crates/genesis-tui/src/lib.rs crates/genesis-tui/src/app.rs
git commit -m "refactor: make welcome screen static"
```

### Task 5: Run targeted verification

**Files:**
- Verify: `crates/genesis-tui/src/widgets/welcome.rs`
- Verify: `crates/genesis-tui/src/lib.rs`
- Verify: `crates/genesis-tui/src/app.rs`

**Step 1: Run welcome-specific tests**

Run: `cargo test -p genesis-tui welcome_widget -- --nocapture`
Expected: PASS

**Step 2: Run full TUI suite**

Run: `cargo test -p genesis-tui`
Expected: PASS

**Step 3: Run CLI coverage path**

Run: `cargo test -p genesis-cli --no-default-features`
Expected: PASS

**Step 4: Commit**

```bash
git add crates/genesis-tui/src/widgets/welcome.rs crates/genesis-tui/src/lib.rs crates/genesis-tui/src/app.rs
git commit -m "test: verify centered welcome dashboard"
```

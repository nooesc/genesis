# Welcome Screen (Animated Monochrome Eve Image + Session Metadata) Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan.

**Goal:** Replace the failed hand-authored ASCII welcome portrait with an image-derived Eve animation rendered as terminal half-block art, while preserving the richer session metadata and key hints already added to the startup screen.

**Architecture:** Keep welcome composition inside `WelcomeWidget`, but stop treating the portrait as authored text. Instead, bundle three source image frames in the repo, decode them once, convert them into ratatui lines using the existing half-block image renderer in `genesis-ui`, and cycle frames only while the welcome screen is visible. The widget remains responsive across wide, medium, and narrow terminal widths, with text-only fallback below the minimum art width.

**Tech Stack:** Rust + ratatui + crossterm + existing Genesis crates (`genesis-tui`, `genesis-ui`) + bundled PNG assets.

---

## Design Decision

The previous direction used hand-authored ASCII portraits. That was the wrong medium for the visual target. The user explicitly wants the welcome screen to feel closer to `cli-music`, which achieves quality by rendering real images into terminal block characters rather than drawing silhouettes from punctuation.

The selected direction is:

1. Bundle three Eve source frames in the repository.
2. Render them into terminal half-block art using the existing `genesis-ui` pipeline.
3. Convert the image output to a mostly monochrome palette with a single accent color.
4. Animate only on the welcome screen at a restrained frame rate.
5. Fall back to text-only when the terminal is too narrow for the portrait to read cleanly.

This is a material architectural change from “ASCII portrait widget” to “image-backed terminal art widget”, and the plan should reflect that directly.

## Source Asset Strategy

The user’s Downloads directory is only the import source. The application must not depend on `~/Downloads` at runtime.

Requirements:

1. Copy the three approved PNG frames into a committed asset directory under the repository.
2. Give them stable, descriptive names.
3. Load them from the repo at build time or runtime from a deterministic relative path.
4. Do not attempt to read user-specific filesystem locations once the feature is implemented.

Likely asset placement:

- `crates/genesis-ui/assets/welcome/eve_frame_01.png`
- `crates/genesis-ui/assets/welcome/eve_frame_02.png`
- `crates/genesis-ui/assets/welcome/eve_frame_03.png`

This keeps the image-rendering concern close to the crate that already owns the half-block rendering primitives.

## Rendering Model

The correct implementation path is to reuse existing image-to-terminal code instead of inventing a second renderer inside `genesis-tui`.

Existing relevant file:

- `crates/genesis-ui/src/banner/frames.rs`

That renderer already contains the core mechanism for converting images into terminal half-block output. The welcome screen should either:

1. reuse the existing renderer directly, or
2. introduce a small welcome-specific helper in `genesis-ui` that wraps the shared image conversion logic

The second option is usually cleaner because welcome rendering has distinct requirements:

- fixed small canvas sizes
- monochrome/stylized palette mapping
- a single accent color
- three-frame animation instead of a single static banner

The important constraint is architectural: `genesis-tui` should consume a prepared renderable output, not own raw image processing details.

## Palette and Styling

The target is not full-color art. The target is monochrome/stylized terminal art with one accent.

Palette rules:

1. Main body of the image maps to a grayscale or warm-neutral ramp.
2. Background should remain visually quiet and integrate with the current TUI palette.
3. One accent color is allowed for small high-value details only.
4. Accent should be sparse. Good candidates are the eyes or tiny suit/collar highlights.
5. If accent overpowers the portrait, the mapping is wrong.

This keeps the startup screen readable and intentional instead of turning it into a noisy ANSI poster.

## Animation Behavior

Animation should be welcome-only and low-frequency.

Rules:

1. Cycle through the three frames only while the welcome screen is visible.
2. Use a restrained cadence, roughly `2-3 fps`.
3. Stop animation immediately once the app leaves the welcome state.
4. Do not spawn ad hoc background work per frame; integrate with the existing render/event loop timing.
5. If animation timing fails or frame decode/render is unavailable, fall back to the first frame or to text-only.

The goal is subtle motion, not a distracting loop.

## Responsive Layout

The welcome screen still needs the three explicit layout bands:

1. `>= 100 cols` — split layout:
   animated portrait on the left, title + metadata + key hints on the right.
2. `70-99 cols` — compact stacked layout:
   smaller portrait centered above the title and metadata.
3. `< 70 cols` — text-only layout:
   no portrait at all.

The portrait must never squeeze the metadata column until it wraps badly. If the image cannot fit cleanly, the widget should degrade to the simpler mode.

## Data Flow

Existing `WelcomeInfo` enrichment should remain:

- `session_id`
- `model`
- `backend`
- `cwd`
- `tool_count_builtin`
- `tool_count_mcp`
- `skill_count`
- `version`

That data is already the right shape for the richer startup screen. The change is strictly in how the left-side visual content is produced and animated.

## Failure Handling

The welcome screen must degrade safely.

Failure cases to handle:

1. image decode failure
2. missing bundled asset
3. terminal too narrow
4. unexpected render sizing issue

Fallback order:

1. first static frame
2. no portrait, text-only welcome

Broken or partially rendered art is worse than no art.

# Welcome Screen (Centered Text Dashboard) Design

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:writing-plans to create the implementation plan.

**Goal:** Replace the current improvised text-only welcome blob with a clean centered dashboard that feels intentional, is easy to scan, and preserves the most useful startup metadata without any image or animation complexity.

**Architecture:** Keep all welcome behavior inside `WelcomeWidget` in `genesis-tui`. The screen should render as a single centered panel with left-aligned contents, a clear title block, compact metadata rows, a subtle divider, and a concise command legend. No image assets, no banner renderer integration, and no welcome animation state should remain active.

**Tech Stack:** Rust + ratatui + crossterm + existing Genesis TUI widgets.

---

## Design Decision

The image-backed welcome experiment created too much rendering churn for too little value. The startup screen should not be the most fragile part of the TUI. The right move is to simplify hard and make the screen feel deliberate through structure, spacing, and typography instead of terminal art.

The selected direction is:

1. text-only welcome screen
2. single centered panel
3. left-aligned contents inside that panel
4. compressed metadata layout
5. compact command legend
6. one strong call to action at the bottom

This keeps the startup experience polished without introducing more rendering complexity.

## Layout Model

The welcome screen should render as one narrow centered dashboard, not as individually centered text lines.

Structure:

1. `Title block`
   - `>_ Eve`
   - version line or muted subtitle below it

2. `Metadata section`
   - fixed-width label column
   - left-aligned values
   - rows for:
     - session
     - model
     - backend
     - cwd
     - tools
     - skills

3. `Divider`
   - one subtle horizontal rule

4. `Command legend`
   - compact command/action rows
   - should read like a dashboard, not a help manual

5. `Primary footer`
   - `Press any key to start`

The panel itself should be centered within the terminal, but the content inside it should be left-aligned for readability.

## Visual Hierarchy

The current welcome screen fails because every line has nearly the same visual weight. The revised screen needs clear hierarchy:

1. strong title color and weight
2. muted subtitle/secondary metadata
3. normal body text for values
4. subdued labels
5. slightly emphasized CTA

The screen should feel closer to a compact launch dashboard than a dump of debug values.

## Spacing Rules

The layout should avoid both extremes:

- not a dense wall of text
- not a sparse floating blob

Rules:

1. Use one blank line between major sections only.
2. Keep metadata rows tight and regular.
3. Keep command hints compact, ideally one per row with aligned keys.
4. Avoid stacking too many empty lines for centering aesthetics.

## Content Rules

Keep:

- title
- version
- session id
- model
- backend
- cwd
- tool counts
- skill count
- key hints
- start prompt

Do not add:

- art
- animation
- fake terminal frame
- loading gimmicks
- noisy status prose

## Responsive Behavior

Even though the layout is single-panel, it should still degrade intentionally:

1. wide terminals:
   - centered panel at fixed target width
2. medium terminals:
   - same layout with narrower truncation
3. narrow terminals:
   - preserve the same structure, but shorten the cwd and keep labels compact

The layout should not switch modes unless necessary. The key is consistency.

## Failure Handling

There is very little that can fail now. The main concerns are terminal size and string length.

Handle:

1. zero-sized areas by returning early
2. narrow widths with truncation
3. small heights by clipping safely rather than panicking

This design intentionally removes the previous failure-prone asset and animation paths from the welcome experience.

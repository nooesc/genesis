//! Custom terminal wrapper with full-screen alternate-mode rendering and dual-buffer diff rendering.
//!
//! This is a simplified wrapper around ratatui's crossterm backend that provides:
//! - Dual-buffer diff rendering: two `Buffer` objects swapped after each frame,
//!   writing only changed cells to the terminal.
//! - Viewport area tracking: the region ratatui renders into.
//!
//! Derived from Codex CLI's `custom_terminal.rs` (MIT-licensed, see codex-rs).

use std::io::{self, Stdout, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{
    Attribute as CAttribute, Colors, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor,
};
use ratatui::backend::{CrosstermBackend, IntoCrossterm};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use unicode_width::UnicodeWidthStr as _;

/// A custom terminal that owns a crossterm backend and two ratatui buffers.
///
/// The dual-buffer approach lets us diff the previous frame against the current
/// frame and emit only the cells that actually changed, minimising I/O.
pub struct CustomTerminal {
    backend: CrosstermBackend<Stdout>,
    buffers: [Buffer; 2],
    current: usize,
    viewport_area: Rect,
    /// Deferred resize: stored on `set_viewport_area`, applied by
    /// `apply_pending_resize` just before the next frame render. This lets
    /// rapid resize events (tmux drag-resize) coalesce so only one screen
    /// clear + full redraw happens per frame interval.
    pending_resize: Option<Rect>,
}

impl CustomTerminal {
    /// Create a new `CustomTerminal` with the given viewport dimensions.
    ///
    /// The viewport starts at row 0 and column 0.
    pub fn new(width: u16, height: u16) -> io::Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        let area = Rect::new(0, 0, width, height);
        Ok(Self {
            backend,
            buffers: [Buffer::empty(area), Buffer::empty(area)],
            current: 0,
            viewport_area: area,
            pending_resize: None,
        })
    }

    /// The area of the viewport that ratatui renders into.
    pub fn viewport_area(&self) -> Rect {
        self.viewport_area
    }

    /// Record a pending viewport resize without clearing the screen yet.
    ///
    /// The actual buffer resize and screen clear happen in [`apply_pending_resize`],
    /// which the render loop calls just before drawing a frame. This lets rapid
    /// resize events (common during tmux drag-resize) coalesce so only one
    /// clear+redraw cycle occurs per frame interval.
    pub fn set_viewport_area(&mut self, area: Rect) {
        if area == self.viewport_area && self.pending_resize.is_none() {
            return;
        }
        self.pending_resize = Some(area);
    }

    /// Apply a pending viewport resize, clearing the terminal screen and
    /// resizing both buffers so that `draw_diff` redraws every cell from
    /// scratch.
    ///
    /// Returns `true` if a resize was applied (caller should reclamp scroll,
    /// cancel effects, etc.), `false` if no resize was pending.
    ///
    /// The screen clear prevents ghost artifacts in multiplexers (tmux/Zellij)
    /// where pane resize can leave orphaned characters that the diff renderer
    /// would never overwrite.
    pub fn apply_pending_resize(&mut self) -> bool {
        let Some(area) = self.pending_resize.take() else {
            return false;
        };
        if area == self.viewport_area {
            return false;
        }
        // Clear the physical terminal so stale pre-resize content is erased.
        let _ = write!(self.backend, "\x1b[2J\x1b[H");
        let _ = self.backend.flush();

        self.buffers[self.current].resize(area);
        self.buffers[1 - self.current].resize(area);
        // Reset BOTH buffers so draw_diff treats every cell as changed.
        self.buffers[self.current].reset();
        self.buffers[1 - self.current].reset();
        self.viewport_area = area;
        true
    }

    /// Get a mutable reference to the current (front) buffer for drawing into.
    pub fn current_buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.current]
    }

    /// Swap the current and previous buffers.
    ///
    /// The newly-previous buffer (the one we just drew) is reset to blank so
    /// that the *next* frame starts clean.  This matches ratatui's convention:
    /// after swap, `current` points to a blank buffer ready for the next render
    /// pass, and the buffer at `1 - current` holds the just-flushed content for
    /// diffing.
    pub fn swap_buffers(&mut self) {
        self.buffers[1 - self.current].reset();
        self.current = 1 - self.current;
    }

    /// Direct mutable access to the crossterm backend for raw ANSI writes.
    pub fn backend_mut(&mut self) -> &mut CrosstermBackend<Stdout> {
        &mut self.backend
    }

    /// Flush the backend's write buffer to stdout.
    pub fn flush(&mut self) -> io::Result<()> {
        self.backend.flush()
    }

    /// Diff the previous buffer against the current buffer and write only the
    /// changed cells to the terminal.
    ///
    /// **Important**: `Buffer::diff` returns `(x, y)` coordinates.
    pub fn draw_diff(&mut self) -> io::Result<()> {
        let previous = &self.buffers[1 - self.current];
        let current = &self.buffers[self.current];
        let updates = previous.diff(current);

        if updates.is_empty() {
            return Ok(());
        }

        let mut fg = Color::Reset;
        let mut bg = Color::Reset;
        let mut modifier = Modifier::empty();
        let mut last_pos: Option<(u16, u16)> = None;

        for (x, y, cell) in &updates {
            // Move cursor only when not adjacent to the previous position.
            // After writing a cell with display width `w`, the terminal cursor
            // is at column `x + w`, not `x + 1`. Track the actual cursor
            // column to avoid unnecessary MoveTo commands for wide characters.
            let need_move = match last_pos {
                Some((cursor_x, cursor_y)) => !(*x == cursor_x && *y == cursor_y),
                None => true,
            };
            if need_move {
                queue!(self.backend, MoveTo(*x, *y))?;
            }
            let cell_width = cell.symbol().width().max(1) as u16;
            last_pos = Some((*x + cell_width, *y));

            // Apply modifier changes.
            if cell.modifier != modifier {
                let diff = ModifierDiff {
                    from: modifier,
                    to: cell.modifier,
                };
                diff.queue(&mut self.backend)?;
                modifier = cell.modifier;
            }

            // Apply color changes.
            if cell.fg != fg || cell.bg != bg {
                queue!(
                    self.backend,
                    SetColors(Colors::new(
                        cell.fg.into_crossterm(),
                        cell.bg.into_crossterm(),
                    ))
                )?;
                fg = cell.fg;
                bg = cell.bg;
            }

            queue!(self.backend, Print(cell.symbol()))?;
        }

        // Reset terminal style state after drawing.
        queue!(
            self.backend,
            SetForegroundColor(crossterm::style::Color::Reset),
            SetBackgroundColor(crossterm::style::Color::Reset),
            SetAttribute(CAttribute::Reset),
        )?;

        // Note: caller is responsible for flushing via `self.flush()`.
        Ok(())
    }

    /// Hard-reset: clear the entire screen and scrollback, reset style state.
    ///
    /// Uses raw ANSI sequences for maximum compatibility:
    /// - `\x1b[r`    -- reset scroll region
    /// - `\x1b[0m`   -- reset attributes
    /// - `\x1b[H`    -- cursor home
    /// - `\x1b[2J`   -- clear visible screen
    /// - `\x1b[3J`   -- clear scrollback
    /// - `\x1b[H`    -- cursor home again
    pub fn clear_all(&mut self) -> io::Result<()> {
        write!(self.backend, "\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H")?;
        self.backend.flush()?;
        // Reset the previous buffer so the next draw_diff redraws everything.
        self.buffers[1 - self.current].reset();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ModifierDiff -- efficient modifier-change emission
// ---------------------------------------------------------------------------

/// Calculates the crossterm attribute commands needed to transition from one
/// set of ratatui modifiers to another.
struct ModifierDiff {
    from: Modifier,
    to: Modifier,
}

impl ModifierDiff {
    fn queue<W: io::Write>(self, w: &mut W) -> io::Result<()> {
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `CustomTerminal`-like pair of buffers without needing
    /// real stdout.  We test buffer logic directly.
    fn make_buffers(area: Rect) -> [Buffer; 2] {
        [Buffer::empty(area), Buffer::empty(area)]
    }

    #[test]
    fn viewport_starts_at_given_size() {
        // We can't call CustomTerminal::new in CI (no tty), so verify the
        // logic by constructing the struct fields directly.
        let area = Rect::new(0, 0, 80, 24);
        let buffers = make_buffers(area);
        let ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers,
            current: 0,
            viewport_area: area,
            pending_resize: None,
        };
        assert_eq!(ct.viewport_area(), Rect::new(0, 0, 80, 24));
    }

    #[test]
    fn set_viewport_area_defers_resize() {
        let initial = Rect::new(0, 0, 80, 24);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(initial),
            current: 0,
            viewport_area: initial,
            pending_resize: None,
        };

        let new_area = Rect::new(0, 0, 120, 30);
        ct.set_viewport_area(new_area);

        // Resize is deferred — viewport_area should NOT have changed yet.
        assert_eq!(ct.viewport_area(), initial);
        assert!(ct.pending_resize.is_some());

        // Apply the pending resize.
        assert!(ct.apply_pending_resize());
        assert_eq!(ct.viewport_area(), new_area);
        assert_eq!(*ct.buffers[0].area(), new_area);
        assert_eq!(*ct.buffers[1].area(), new_area);
    }

    #[test]
    fn set_viewport_area_no_op_for_same_size() {
        let area = Rect::new(0, 0, 80, 24);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(area),
            current: 0,
            viewport_area: area,
            pending_resize: None,
        };

        ct.set_viewport_area(area);
        assert!(ct.pending_resize.is_none());
        assert!(!ct.apply_pending_resize());
    }

    #[test]
    fn rapid_resizes_coalesce_to_final() {
        let initial = Rect::new(0, 0, 80, 24);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(initial),
            current: 0,
            viewport_area: initial,
            pending_resize: None,
        };

        // Simulate rapid resize events (like tmux drag-resize).
        ct.set_viewport_area(Rect::new(0, 0, 90, 24));
        ct.set_viewport_area(Rect::new(0, 0, 100, 24));
        ct.set_viewport_area(Rect::new(0, 0, 110, 30));

        // Only one apply should happen, with the final dimensions.
        let final_area = Rect::new(0, 0, 110, 30);
        assert!(ct.apply_pending_resize());
        assert_eq!(ct.viewport_area(), final_area);
        // Second apply should be a no-op.
        assert!(!ct.apply_pending_resize());
    }

    #[test]
    fn swap_buffers_alternates() {
        let area = Rect::new(0, 0, 10, 5);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(area),
            current: 0,
            viewport_area: area,
            pending_resize: None,
        };

        assert_eq!(ct.current, 0);
        ct.swap_buffers();
        assert_eq!(ct.current, 1);
        ct.swap_buffers();
        assert_eq!(ct.current, 0);
    }

    #[test]
    fn diff_empty_buffers_produces_no_updates() {
        let area = Rect::new(0, 0, 10, 5);
        let prev = Buffer::empty(area);
        let curr = Buffer::empty(area);
        let updates = prev.diff(&curr);
        assert!(updates.is_empty());
    }

    #[test]
    fn diff_detects_changed_cell() {
        let area = Rect::new(0, 0, 10, 5);
        let prev = Buffer::empty(area);
        let mut curr = Buffer::empty(area);
        curr.cell_mut((3, 2))
            .expect("cell should exist")
            .set_symbol("X");

        let updates = prev.diff(&curr);
        assert!(!updates.is_empty());
        // The coordinate from diff should be (3, 2) -- absolute, not offset.
        let (x, y, cell) = &updates[0];
        assert_eq!(*x, 3);
        assert_eq!(*y, 2);
        assert_eq!(cell.symbol(), "X");
    }
}

//! Custom terminal wrapper with inline viewport and dual-buffer diff rendering.
//!
//! This is a simplified wrapper around ratatui's crossterm backend that provides:
//! - Dual-buffer diff rendering: two `Buffer` objects swapped after each frame,
//!   writing only changed cells to the terminal.
//! - Viewport area tracking: the region ratatui renders into (may start at y > 0
//!   for inline viewports).
//! - History row tracking: how many rows above the viewport have been pushed into
//!   terminal scrollback.
//!
//! Derived from Codex CLI's `custom_terminal.rs` (MIT-licensed, see codex-rs).

use std::io::{self, Stdout, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{
    Attribute as CAttribute, Colors, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor,
};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

/// A custom terminal that owns a crossterm backend and two ratatui buffers.
///
/// The dual-buffer approach lets us diff the previous frame against the current
/// frame and emit only the cells that actually changed, minimising I/O.
pub struct CustomTerminal {
    backend: CrosstermBackend<Stdout>,
    buffers: [Buffer; 2],
    current: usize,
    viewport_area: Rect,
    visible_history_rows: u16,
}

impl CustomTerminal {
    /// Create a new `CustomTerminal` with the given viewport dimensions.
    ///
    /// The viewport starts at row 0 and column 0.  Call [`set_viewport_area`]
    /// to move it (e.g. after determining the cursor position for an inline
    /// viewport).
    pub fn new(width: u16, height: u16) -> io::Result<Self> {
        let backend = CrosstermBackend::new(io::stdout());
        let area = Rect::new(0, 0, width, height);
        Ok(Self {
            backend,
            buffers: [Buffer::empty(area), Buffer::empty(area)],
            current: 0,
            viewport_area: area,
            visible_history_rows: 0,
        })
    }

    /// The area of the viewport that ratatui renders into.
    pub fn viewport_area(&self) -> Rect {
        self.viewport_area
    }

    /// Number of rows above the viewport that have been pushed into scrollback.
    pub fn visible_history_rows(&self) -> u16 {
        self.visible_history_rows
    }

    /// Update the viewport area and resize both buffers to match.
    ///
    /// History rows are clamped so they never exceed `area.y` (the viewport's
    /// top edge, which is the maximum number of scrollback rows that can exist
    /// above it).
    pub fn set_viewport_area(&mut self, area: Rect) {
        self.buffers[self.current].resize(area);
        self.buffers[1 - self.current].resize(area);
        self.viewport_area = area;
        self.visible_history_rows = self.visible_history_rows.min(area.y);
    }

    /// Record that `rows` new lines have been pushed into scrollback above the
    /// viewport (e.g. via DECSTBM scroll-region insertion).
    ///
    /// The total is clamped to `viewport_area.y` so we never claim more history
    /// rows than physically exist above the viewport.
    pub fn note_history_rows_inserted(&mut self, rows: u16) {
        self.visible_history_rows = self
            .visible_history_rows
            .saturating_add(rows)
            .min(self.viewport_area.y);
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
    /// **Important**: `Buffer::diff` returns `(x, y)` coordinates that are
    /// *already* absolute screen positions (because the buffers are created with
    /// `Buffer::empty(viewport_area)` where `viewport_area.y` may be > 0).
    /// We must **not** add the viewport offset again.
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
            let need_move = match last_pos {
                Some((px, py)) => !(*x == px + 1 && *y == py),
                None => true,
            };
            if need_move {
                queue!(self.backend, MoveTo(*x, *y))?;
            }
            last_pos = Some((*x, *y));

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
                    SetColors(Colors::new(cell.fg.into(), cell.bg.into()))
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

        self.backend.flush()
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
        self.visible_history_rows = 0;
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
            visible_history_rows: 0,
        };
        assert_eq!(ct.viewport_area(), Rect::new(0, 0, 80, 24));
        assert_eq!(ct.visible_history_rows(), 0);
    }

    #[test]
    fn set_viewport_area_resizes_buffers() {
        let initial = Rect::new(0, 0, 80, 24);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(initial),
            current: 0,
            viewport_area: initial,
            visible_history_rows: 0,
        };

        let new_area = Rect::new(0, 5, 120, 30);
        ct.set_viewport_area(new_area);

        assert_eq!(ct.viewport_area(), new_area);
        assert_eq!(*ct.buffers[0].area(), new_area);
        assert_eq!(*ct.buffers[1].area(), new_area);
    }

    #[test]
    fn note_history_rows_clamped_to_viewport_top() {
        // Viewport starts at row 10, so at most 10 history rows can exist.
        let area = Rect::new(0, 10, 80, 14);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(area),
            current: 0,
            viewport_area: area,
            visible_history_rows: 0,
        };

        ct.note_history_rows_inserted(5);
        assert_eq!(ct.visible_history_rows(), 5);

        ct.note_history_rows_inserted(3);
        assert_eq!(ct.visible_history_rows(), 8);

        // Would exceed viewport top (10) -- should clamp.
        ct.note_history_rows_inserted(100);
        assert_eq!(ct.visible_history_rows(), 10);
    }

    #[test]
    fn swap_buffers_alternates() {
        let area = Rect::new(0, 0, 10, 5);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(area),
            current: 0,
            viewport_area: area,
            visible_history_rows: 0,
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

    #[test]
    fn diff_with_offset_viewport_returns_absolute_coords() {
        // Viewport starting at row 5 -- diff coordinates include that offset.
        let area = Rect::new(0, 5, 10, 3);
        let prev = Buffer::empty(area);
        let mut curr = Buffer::empty(area);
        curr.cell_mut((2, 5))
            .expect("cell should exist")
            .set_symbol("A");

        let updates = prev.diff(&curr);
        assert!(!updates.is_empty());
        let (x, y, cell) = &updates[0];
        assert_eq!(*x, 2);
        assert_eq!(*y, 5); // absolute row, not 0
        assert_eq!(cell.symbol(), "A");
    }

    #[test]
    fn set_viewport_area_clamps_history_rows() {
        let area = Rect::new(0, 20, 80, 4);
        let mut ct = CustomTerminal {
            backend: CrosstermBackend::new(io::stdout()),
            buffers: make_buffers(area),
            current: 0,
            viewport_area: area,
            visible_history_rows: 15,
        };

        // Move viewport up -- history rows should be clamped.
        let new_area = Rect::new(0, 8, 80, 16);
        ct.set_viewport_area(new_area);
        assert_eq!(ct.visible_history_rows(), 8);
    }
}

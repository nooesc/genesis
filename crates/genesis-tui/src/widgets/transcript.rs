//! Transcript overlay — fullscreen scrollable view of the complete conversation history.
//!
//! Rendered over the full viewport when the user presses Ctrl+T. All
//! committed [`HistoryCell`]s are converted to styled [`Line`]s once at
//! construction time and then scrolled with vim-like keybindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget as _},
};

use crate::history::cell::HistoryCell;

/// Action returned by [`TranscriptOverlay::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptAction {
    /// No state change that requires the caller to act.
    None,
    /// Close the overlay and return to normal chat view.
    Close,
}

/// Fullscreen scrollable overlay showing the complete conversation history.
pub struct TranscriptOverlay {
    /// All conversation cells rendered as styled lines.
    lines: Vec<Line<'static>>,
    /// First visible line index (0-based, from top of the full line list).
    scroll_offset: usize,
    /// Total number of lines (cached from `lines.len()` for convenience).
    total_lines: usize,
}

impl TranscriptOverlay {
    /// Build a transcript from a slice of committed [`HistoryCell`]s.
    ///
    /// Each cell is rendered via [`HistoryCell::to_scrollback_lines`] and
    /// separated by a blank line. Scrolling starts at the bottom so the user
    /// sees the most recent messages first.
    pub fn from_cells(cells: &[HistoryCell], width: u16, visible_rows: u16) -> Self {
        let mut lines: Vec<Line<'static>> = Vec::new();
        for cell in cells {
            lines.extend(cell.to_scrollback_lines(width));
            lines.push(Line::default()); // blank separator between cells
        }
        let total = lines.len();
        Self {
            scroll_offset: total.saturating_sub(visible_rows as usize),
            lines,
            total_lines: total,
        }
    }

    /// Handle a key event and return the resulting [`TranscriptAction`].
    pub fn handle_key(&mut self, key: KeyEvent, visible_rows: u16) -> TranscriptAction {
        let half_page = (visible_rows as usize / 2).max(1);
        match key.code {
            // ── Scroll down ──────────────────────────────────────────────
            KeyCode::Char('j') | KeyCode::Down => self.scroll_down(1, visible_rows),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_down(half_page, visible_rows)
            }
            KeyCode::PageDown => self.scroll_down(visible_rows as usize, visible_rows),

            // ── Scroll up ────────────────────────────────────────────────
            KeyCode::Char('k') | KeyCode::Up => self.scroll_up(1),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_up(half_page)
            }
            KeyCode::PageUp => self.scroll_up(visible_rows as usize),

            // ── Jump to extremes ─────────────────────────────────────────
            KeyCode::Char('g') => self.scroll_to_top(),
            KeyCode::Char('G') => self.scroll_to_bottom(visible_rows),
            KeyCode::Home => self.scroll_to_top(),
            KeyCode::End => self.scroll_to_bottom(visible_rows),

            // ── Close ────────────────────────────────────────────────────
            KeyCode::Char('q') => return TranscriptAction::Close,
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return TranscriptAction::Close
            }
            KeyCode::Esc => return TranscriptAction::Close,

            _ => {}
        }
        TranscriptAction::None
    }

    /// Render the overlay into the given area.
    ///
    /// Layout:
    /// - Row 0: header bar with keybinding hints and scroll position indicator.
    /// - Rows 1..: visible conversation lines.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        // ── Header ────────────────────────────────────────────────────────
        let header_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        self.render_header(header_area, buf);

        if area.height < 2 {
            return;
        }

        // ── Content ───────────────────────────────────────────────────────
        let content_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height - 1,
        };
        self.render_content(content_area, buf);
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn scroll_down(&mut self, n: usize, visible_rows: u16) {
        let max = self.total_lines.saturating_sub(visible_rows as usize);
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self, visible_rows: u16) {
        self.scroll_offset = self.total_lines.saturating_sub(visible_rows as usize);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let hint = " Transcript — j/k scroll, Ctrl+D/U page, g/G top/bottom, q to close ";
        let pos_text = if self.total_lines == 0 {
            "[empty]".to_owned()
        } else {
            format!("[{}/{}]", self.scroll_offset + 1, self.total_lines)
        };

        // Left-aligned hint.
        let hint_span = Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
        // Right-aligned position indicator.
        let pos_span = Span::styled(
            pos_text.clone(),
            Style::default().fg(Color::DarkGray),
        );

        // Compute how much padding is needed between hint and position.
        let hint_len = hint.len() as u16;
        let pos_len = pos_text.len() as u16;
        let total = area.width;
        let pad = total.saturating_sub(hint_len + pos_len);
        let padding = Span::raw(" ".repeat(pad as usize));

        let header_line = Line::from(vec![hint_span, padding, pos_span]);
        let paragraph = Paragraph::new(header_line);
        paragraph.render(area, buf);
    }

    fn render_content(&self, area: Rect, buf: &mut Buffer) {
        let visible_rows = area.height as usize;
        let start = self.scroll_offset;
        let end = (start + visible_rows).min(self.total_lines);

        let visible: Vec<Line<'static>> = if start < self.total_lines {
            self.lines[start..end].to_vec()
        } else {
            Vec::new()
        };

        let paragraph = Paragraph::new(visible);
        paragraph.render(area, buf);
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::history::{AgentCell, HistoryCell, UserCell};

    use super::*;

    fn make_cells() -> Vec<HistoryCell> {
        vec![
            HistoryCell::User(UserCell::new("hello")),
            HistoryCell::Agent(AgentCell::new("hi there, I am Eve")),
        ]
    }

    /// Build a transcript and verify that all cell lines (plus blank separators)
    /// are present.
    #[test]
    fn from_cells_renders_all() {
        let cells = make_cells();
        let t = TranscriptOverlay::from_cells(&cells, 80, 24);
        // 2 cells → 2 content lines + 2 blank separators = 4 total lines
        assert_eq!(t.total_lines, 4);
        assert_eq!(t.lines.len(), 4);
    }

    /// Scrolling down increments the offset by one.
    #[test]
    fn scroll_down_advances() {
        // Use enough cells so total_lines > visible_rows (3 visible).
        let mut cells = Vec::new();
        for i in 0..10 {
            cells.push(HistoryCell::Agent(AgentCell::new(format!("msg {i}"))));
        }
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 3);
        t.scroll_to_top(); // start from 0
        let before = t.scroll_offset;
        t.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), 3);
        assert_eq!(t.scroll_offset, before + 1);
    }

    /// Scrolling up decrements the offset by one (clamped at 0).
    #[test]
    fn scroll_up_goes_back() {
        // Use enough cells so total_lines > visible_rows (3 visible).
        let mut cells = Vec::new();
        for i in 0..10 {
            cells.push(HistoryCell::Agent(AgentCell::new(format!("msg {i}"))));
        }
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 3);
        t.scroll_to_top();
        // At offset 0, scrolling up should not underflow.
        t.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), 3);
        assert_eq!(t.scroll_offset, 0);

        // Move down first, then up.
        t.scroll_down(2, 3);
        let offset_before = t.scroll_offset;
        t.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), 3);
        assert_eq!(t.scroll_offset, offset_before - 1);
    }

    /// `g` / `G` jump to top and bottom.
    #[test]
    fn scroll_to_top_and_bottom() {
        let cells = make_cells();
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 24);

        t.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE), 24);
        assert_eq!(t.scroll_offset, 0);

        t.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE), 24);
        assert_eq!(t.scroll_offset, t.total_lines.saturating_sub(24));
    }

    /// `from_cells` starts at the bottom (most recent).
    #[test]
    fn starts_at_bottom() {
        let cells = make_cells();
        let t = TranscriptOverlay::from_cells(&cells, 80, 24);
        assert_eq!(t.scroll_offset, t.total_lines.saturating_sub(24));
    }

    /// `q` returns `TranscriptAction::Close`.
    #[test]
    fn q_closes() {
        let cells = make_cells();
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 24);
        let action =
            t.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), 24);
        assert_eq!(action, TranscriptAction::Close);
    }

    /// `Esc` returns `TranscriptAction::Close`.
    #[test]
    fn esc_closes() {
        let cells = make_cells();
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 24);
        let action = t.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(action, TranscriptAction::Close);
    }

    /// `Ctrl+T` returns `TranscriptAction::Close`.
    #[test]
    fn ctrl_t_closes() {
        let cells = make_cells();
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 24);
        let action = t.handle_key(
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
            24,
        );
        assert_eq!(action, TranscriptAction::Close);
    }

    /// Ctrl+D scrolls by half the visible rows.
    #[test]
    fn ctrl_d_half_page() {
        let mut cells = Vec::new();
        for i in 0..20 {
            cells.push(HistoryCell::Agent(AgentCell::new(format!("line {i}"))));
        }
        let mut t = TranscriptOverlay::from_cells(&cells, 80, 24);
        t.scroll_to_top();
        let before = t.scroll_offset;
        t.handle_key(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
            24,
        );
        // half of 24 = 12
        assert_eq!(t.scroll_offset, before + 12);
    }

    /// Empty cell list creates an overlay without panicking.
    #[test]
    fn from_empty_cells() {
        let t = TranscriptOverlay::from_cells(&[], 80, 24);
        assert_eq!(t.total_lines, 0);
        assert_eq!(t.scroll_offset, 0);
    }

    /// Tool cells are included in the rendered transcript.
    #[test]
    fn from_cells_includes_tool_cells() {
        use crate::history::ToolCell;
        let cells = vec![HistoryCell::Tool(ToolCell::new(
            "shell",
            "c1",
            "ls",
            true,
            Duration::from_millis(100),
        ))];
        let t = TranscriptOverlay::from_cells(&cells, 80, 24);
        // ToolCell with default Grouped mode produces 4 lines + 1 blank = 5
        assert_eq!(t.total_lines, 5);
    }
}

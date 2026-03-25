//! Help overlay — fullscreen scrollable view of keybindings and commands.
//!
//! Rendered over the full viewport when the user types `/help`.
//! Follows the same pattern as the transcript overlay with vim-like scrolling.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget as _},
};

/// Action returned by [`HelpOverlay::handle_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpAction {
    /// No state change.
    None,
    /// Close the overlay.
    Close,
}

/// Fullscreen scrollable overlay showing keybindings and available commands.
pub struct HelpOverlay {
    /// Pre-rendered styled lines.
    lines: Vec<Line<'static>>,
    /// First visible line index.
    scroll_offset: usize,
    /// Total line count.
    total_lines: usize,
}

// ── Palette (consistent with status bar) ────────────────────────────────────

const ACCENT: Color = Color::Rgb(180, 167, 214);
const DIM: Color = Color::Rgb(98, 98, 98);
const TEXT: Color = Color::Rgb(168, 168, 168);
const KEY: Color = Color::Rgb(212, 165, 116);

impl Default for HelpOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl HelpOverlay {
    /// Build the help content.
    pub fn new() -> Self {
        let lines = Self::build_content();
        let total = lines.len();
        Self {
            scroll_offset: 0,
            lines,
            total_lines: total,
        }
    }

    fn build_content() -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        let accent_style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
        let key_style = Style::default().fg(KEY).add_modifier(Modifier::BOLD);
        let desc_style = Style::default().fg(TEXT);
        let dim_style = Style::default().fg(DIM);

        // Title
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("  ◆ ", Style::default().fg(ACCENT)),
            Span::styled("Eve — Genesis TUI Help", accent_style),
        ]));
        lines.push(Line::default());

        // Keybindings section
        lines.push(Line::from(Span::styled("  Keybindings", accent_style)));
        lines.push(Line::from(Span::styled("  ───────────", dim_style)));

        let bindings: &[(&str, &str)] = &[
            ("Enter", "Submit message"),
            ("Shift+Enter", "Insert newline (Kitty/WezTerm)"),
            ("Alt+Enter", "Insert newline (most terminals)"),
            ("Ctrl+J", "Insert newline (universal)"),
            ("Ctrl+C", "Interrupt running turn"),
            ("Ctrl+D ×2", "Exit Eve (double-press on empty input)"),
            ("Ctrl+G", "Open $VISUAL/$EDITOR for long prompts"),
            ("Ctrl+L", "Clear conversation and screen"),
            ("Ctrl+P", "Open command palette"),
            ("Ctrl+T", "Toggle transcript overlay"),
            ("Tab", "Toggle Plan/Act mode"),
            ("@", "File path completion"),
            ("Ctrl+Left", "Move cursor to previous word"),
            ("Ctrl+Right", "Move cursor to next word"),
            ("Home/Ctrl+A", "Move cursor to start of line"),
            ("End/Ctrl+E", "Move cursor to end of line"),
            ("Ctrl+B/F", "Move cursor left/right (Emacs)"),
            ("Ctrl+K", "Delete to end of line (fill kill buffer)"),
            ("Ctrl+U", "Delete to start of line (fill kill buffer)"),
            ("Ctrl+W", "Delete word backward (fill kill buffer)"),
            ("Alt+Bksp", "Delete word backward (fill kill buffer)"),
            ("Ctrl+Y", "Yank (paste) last killed text"),
            ("PageUp", "Scroll chat up (half page)"),
            ("PageDown", "Scroll chat down (half page)"),
            ("Shift+Up", "Scroll chat up (1 line)"),
            ("Shift+Down", "Scroll chat down (1 line)"),
            ("Ctrl+End", "Jump to bottom of chat"),
            ("Up/Down", "Navigate input history / lines"),
            ("Esc", "Close overlay / dismiss popup"),
        ];

        for (key, desc) in bindings {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<16}", key), key_style),
                Span::styled((*desc).to_owned(), desc_style),
            ]));
        }

        lines.push(Line::default());

        // Slash commands section
        lines.push(Line::from(Span::styled("  Slash Commands", accent_style)));
        lines.push(Line::from(Span::styled("  ──────────────", dim_style)));

        let commands: &[(&str, &str)] = &[
            ("/clear", "Clear conversation history"),
            ("/compact", "Compact tool display (one-line)"),
            ("/copy", "Copy last response to clipboard"),
            ("/exit", "Exit Eve"),
            ("/grouped", "Grouped tool display (bordered)"),
            ("/help", "Show this help screen"),
            ("/models", "Switch AI model"),
            ("/plan", "Toggle Plan/Act mode"),
            ("/theme", "Cycle through built-in themes"),
            ("/transcript", "View conversation transcript"),
            ("/verbose", "Verbose tool display (full output)"),
        ];

        for (cmd, desc) in commands {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<16}", cmd), key_style),
                Span::styled((*desc).to_owned(), desc_style),
            ]));
        }

        lines.push(Line::default());

        // Overlay navigation
        lines.push(Line::from(Span::styled(
            "  Overlay Navigation",
            accent_style,
        )));
        lines.push(Line::from(Span::styled("  ─────────────────", dim_style)));

        let nav: &[(&str, &str)] = &[
            ("j / Down", "Scroll down one line"),
            ("k / Up", "Scroll up one line"),
            ("Ctrl+D", "Scroll down half page"),
            ("Ctrl+U", "Scroll up half page"),
            ("g / Home", "Jump to top"),
            ("G / End", "Jump to bottom"),
            ("q / Esc", "Close overlay"),
        ];

        for (key, desc) in nav {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<16}", key), key_style),
                Span::styled((*desc).to_owned(), desc_style),
            ]));
        }

        lines.push(Line::default());

        lines
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent, visible_rows: u16) -> HelpAction {
        let half_page = (visible_rows as usize / 2).max(1);
        match key.code {
            // Scroll down
            KeyCode::Char('j') | KeyCode::Down => self.scroll_down(1, visible_rows),
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_down(half_page, visible_rows)
            }
            KeyCode::PageDown => self.scroll_down(visible_rows as usize, visible_rows),

            // Scroll up
            KeyCode::Char('k') | KeyCode::Up => self.scroll_up(1),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.scroll_up(half_page)
            }
            KeyCode::PageUp => self.scroll_up(visible_rows as usize),

            // Jump
            KeyCode::Char('g') => self.scroll_to_top(),
            KeyCode::Char('G') => self.scroll_to_bottom(visible_rows),
            KeyCode::Home => self.scroll_to_top(),
            KeyCode::End => self.scroll_to_bottom(visible_rows),

            // Close
            KeyCode::Char('q') => return HelpAction::Close,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return HelpAction::Close
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return HelpAction::Close
            }
            KeyCode::Esc => return HelpAction::Close,

            _ => {}
        }
        HelpAction::None
    }

    /// Render the overlay into the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        // Header
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

        // Content
        let content_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height - 1,
        };
        self.render_content(content_area, buf);
    }

    // ── Scroll helpers (public for mouse-scroll routing from App) ────────

    pub fn scroll_down(&mut self, n: usize, visible_rows: u16) {
        let max = self.total_lines.saturating_sub(visible_rows as usize);
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    fn scroll_to_bottom(&mut self, visible_rows: u16) {
        self.scroll_offset = self.total_lines.saturating_sub(visible_rows as usize);
    }

    fn render_header(&self, area: Rect, buf: &mut Buffer) {
        let hint = " Help — j/k scroll, Ctrl+D/U page, g/G top/bottom, q to close ";
        let hint_span = Span::styled(
            hint,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
        let header_line = Line::from(vec![hint_span]);
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
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn new_creates_non_empty_content() {
        let overlay = HelpOverlay::new();
        assert!(overlay.total_lines > 0);
    }

    #[test]
    fn starts_at_top() {
        let overlay = HelpOverlay::new();
        assert_eq!(overlay.scroll_offset, 0);
    }

    #[test]
    fn q_closes() {
        let mut overlay = HelpOverlay::new();
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE), 24);
        assert_eq!(action, HelpAction::Close);
    }

    #[test]
    fn esc_closes() {
        let mut overlay = HelpOverlay::new();
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(action, HelpAction::Close);
    }

    #[test]
    fn scroll_down_increments() {
        let mut overlay = HelpOverlay::new();
        overlay.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), 5);
        assert_eq!(overlay.scroll_offset, 1);
    }

    #[test]
    fn scroll_up_at_top_stays() {
        let mut overlay = HelpOverlay::new();
        overlay.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), 24);
        assert_eq!(overlay.scroll_offset, 0);
    }

    #[test]
    fn content_includes_keybindings_section() {
        let overlay = HelpOverlay::new();
        let text: String = overlay
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.contains("Keybindings"),
            "should contain keybindings section"
        );
        assert!(text.contains("Ctrl+C"), "should list Ctrl+C");
        assert!(text.contains("Ctrl+G"), "should list Ctrl+G (editor)");
        assert!(text.contains("Ctrl+L"), "should list Ctrl+L (clear)");
        assert!(
            text.contains("Ctrl+Left"),
            "should list Ctrl+Left (word nav)"
        );
        assert!(text.contains("Ctrl+Y"), "should list Ctrl+Y (yank)");
        assert!(text.contains("/help"), "should list /help command");
        assert!(text.contains("/copy"), "should list /copy command");
    }

    // ── Snapshot tests ────────────────────────────────────────────────

    /// Render a buffer to a string for snapshot testing.
    fn buffer_to_string(buf: &Buffer) -> String {
        let area = buf.area();
        let mut output = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = buf.cell((x, y)).unwrap();
                output.push_str(cell.symbol());
            }
            output.push('\n');
        }
        output
    }

    #[test]
    fn snapshot_help_overlay() {
        let overlay = HelpOverlay::new();
        let area = Rect::new(0, 0, 60, 55);
        let mut buf = Buffer::empty(area);
        overlay.render(area, &mut buf);
        insta::assert_snapshot!(buffer_to_string(&buf));
    }
}

//! Text input widget with readline-style editing and command history.
//!
//! Renders a single-line input as `you> {text}` with a block cursor
//! on the character under the cursor position.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// The prefix shown before user input (includes trailing space).
const PREFIX: &str = "you> ";

/// Style for the `you> ` prefix.
const PREFIX_STYLE: Style = Style::new().fg(Color::Rgb(108, 108, 108));

/// Style for normal (non-cursor) input text.
const TEXT_STYLE: Style = Style::new().fg(Color::Rgb(208, 208, 208));

/// Style for the character under the cursor.
const CURSOR_STYLE: Style = Style::new()
    .fg(Color::Rgb(208, 208, 208))
    .add_modifier(Modifier::REVERSED);

/// Action returned from [`InputWidget::handle_key`].
#[derive(Debug, PartialEq, Eq)]
pub enum InputAction {
    /// No action needed — the widget consumed the key.
    None,
    /// User pressed Enter; contains the submitted text.
    Submit(String),
    /// User pressed Ctrl+C (interrupt the running turn).
    Interrupt,
    /// User pressed Ctrl+D on an empty buffer (request exit).
    Exit,
}

/// Single-line text input with readline-style key bindings and
/// command history navigation.
pub struct InputWidget {
    /// Current edit buffer.
    buffer: String,
    /// Byte position of the cursor within `buffer`.
    cursor: usize,
    /// Submitted entries kept for Up/Down recall (oldest first).
    history: Vec<String>,
    /// `None` = editing new text; `Some(i)` = viewing `history[i]`.
    history_index: Option<usize>,
    /// Snapshot of the buffer taken when the user first pressed Up,
    /// so we can restore it when they press Down past the end.
    saved_input: Option<String>,
}

impl InputWidget {
    /// Create a new, empty input widget.
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            saved_input: None,
        }
    }

    // ── Public accessors ──────────────────────────────────────────────────

    /// The current text in the edit buffer.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Clear the buffer and reset the cursor.
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Push a new entry onto the history stack (call after each submission).
    ///
    /// Duplicate consecutive entries are silently dropped to avoid cluttering
    /// history when the user re-submits the same command.
    pub fn push_history(&mut self, entry: String) {
        if entry.is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) != Some(&entry) {
            self.history.push(entry);
        }
        // Reset navigation state.
        self.history_index = None;
        self.saved_input = None;
    }

    /// Height of this widget in terminal rows (always 1 for now).
    #[allow(clippy::unused_self)]
    pub fn height(&self, _width: u16) -> u16 {
        1
    }

    // ── Event handling ────────────────────────────────────────────────────

    /// Handle a keyboard event and return the resulting [`InputAction`].
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        match (key.code, key.modifiers) {
            // ── Submit ───────────────────────────────────────────────────
            (KeyCode::Enter, _) => {
                let text = std::mem::take(&mut self.buffer);
                self.cursor = 0;
                self.history_index = None;
                self.saved_input = None;
                InputAction::Submit(text)
            }

            // ── Interrupt / Exit ─────────────────────────────────────────
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => InputAction::Interrupt,

            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                if self.buffer.is_empty() {
                    InputAction::Exit
                } else {
                    self.delete_char_at_cursor();
                    InputAction::None
                }
            }

            // ── Kill / clear line ────────────────────────────────────────
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
                InputAction::None
            }

            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                self.buffer.truncate(self.cursor);
                InputAction::None
            }

            // ── Cursor movement ──────────────────────────────────────────
            (KeyCode::Left, _) => {
                self.move_cursor_left();
                InputAction::None
            }

            (KeyCode::Right, _) => {
                self.move_cursor_right();
                InputAction::None
            }

            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                self.cursor = 0;
                InputAction::None
            }

            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                self.cursor = self.buffer.len();
                InputAction::None
            }

            // ── Delete ───────────────────────────────────────────────────
            (KeyCode::Backspace, _) => {
                self.delete_char_before_cursor();
                InputAction::None
            }

            (KeyCode::Delete, _) => {
                self.delete_char_at_cursor();
                InputAction::None
            }

            // ── History navigation ────────────────────────────────────────
            (KeyCode::Up, _) => {
                self.history_prev();
                InputAction::None
            }

            (KeyCode::Down, _) => {
                self.history_next();
                InputAction::None
            }

            // ── Character insertion ──────────────────────────────────────
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                self.insert_char(c);
                InputAction::None
            }

            _ => InputAction::None,
        }
    }

    /// Handle a bracketed-paste event by inserting all characters.
    ///
    /// Newlines inside the pasted text are treated as spaces to avoid
    /// triggering an accidental submission.
    pub fn handle_paste(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\n' || c == '\r' {
                self.insert_char(' ');
            } else {
                self.insert_char(c);
            }
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Render the input line into `buf` at `area`.
    ///
    /// Layout: `you> {text}` where the character at `cursor` is displayed
    /// with inverted colours. If the cursor is at the end of the text, a
    /// space with inverted colours acts as the block cursor.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let prefix_span = Span::styled(PREFIX, PREFIX_STYLE);

        // Build text spans with cursor highlight.
        let text_spans = self.build_text_spans();

        let line = Line::from(
            std::iter::once(prefix_span)
                .chain(text_spans)
                .collect::<Vec<_>>(),
        );

        // Render the line into the first row of the area.
        let row = area.y;
        let mut x = area.x;

        for span in &line.spans {
            let style = span.style;
            for ch in span.content.chars() {
                if x >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, row)) {
                    let mut s = String::with_capacity(ch.len_utf8());
                    s.push(ch);
                    cell.set_symbol(&s);
                    cell.set_style(style);
                }
                x += 1;
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Build the spans for the text portion including the cursor highlight.
    fn build_text_spans(&self) -> Vec<Span<'static>> {
        let text = &self.buffer;

        if text.is_empty() {
            // Show a block cursor on a space at position 0.
            return vec![Span::styled(" ", CURSOR_STYLE)];
        }

        let mut spans: Vec<Span<'static>> = Vec::new();

        // Text before the cursor.
        if self.cursor > 0 {
            spans.push(Span::styled(
                text[..self.cursor].to_owned(),
                TEXT_STYLE,
            ));
        }

        // Character at the cursor (with reversed style).
        if self.cursor < text.len() {
            // Find the end of the character at cursor.
            let char_end = text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(text.len());

            spans.push(Span::styled(
                text[self.cursor..char_end].to_owned(),
                CURSOR_STYLE,
            ));

            // Text after the cursor.
            if char_end < text.len() {
                spans.push(Span::styled(text[char_end..].to_owned(), TEXT_STYLE));
            }
        } else {
            // Cursor is past the end — add a block cursor space.
            spans.push(Span::styled(" ", CURSOR_STYLE));
        }

        spans
    }

    /// Insert `c` at the current cursor byte position.
    fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character immediately before the cursor.
    fn delete_char_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Walk back one char boundary.
        let prev = self.prev_char_boundary(self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Delete the character at the cursor (like Delete key / Ctrl+D).
    fn delete_char_at_cursor(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next = self.next_char_boundary(self.cursor);
        self.buffer.drain(self.cursor..next);
        // cursor stays at the same position.
    }

    /// Move cursor left by one Unicode scalar value.
    fn move_cursor_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.prev_char_boundary(self.cursor);
    }

    /// Move cursor right by one Unicode scalar value.
    fn move_cursor_right(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        self.cursor = self.next_char_boundary(self.cursor);
    }

    /// Return the byte offset of the previous char boundary before `pos`.
    fn prev_char_boundary(&self, pos: usize) -> usize {
        let mut p = pos;
        loop {
            p -= 1;
            if self.buffer.is_char_boundary(p) {
                return p;
            }
        }
    }

    /// Return the byte offset of the next char boundary after `pos`.
    fn next_char_boundary(&self, pos: usize) -> usize {
        let mut p = pos + 1;
        while p <= self.buffer.len() && !self.buffer.is_char_boundary(p) {
            p += 1;
        }
        p
    }

    /// Navigate to the previous (older) history entry.
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }

        let new_index = match self.history_index {
            None => {
                // Save current input before navigating away.
                self.saved_input = Some(self.buffer.clone());
                self.history.len() - 1
            }
            Some(0) => 0, // Already at oldest — stay.
            Some(i) => i - 1,
        };

        self.history_index = Some(new_index);
        self.buffer = self.history[new_index].clone();
        self.cursor = self.buffer.len();
    }

    /// Navigate to the next (newer) history entry, or back to saved input.
    fn history_next(&mut self) {
        match self.history_index {
            None => {} // Already editing new text; nothing to do.
            Some(i) if i + 1 >= self.history.len() => {
                // Reached the end of history — restore the saved buffer.
                self.history_index = None;
                self.buffer = self.saved_input.take().unwrap_or_default();
                self.cursor = self.buffer.len();
            }
            Some(i) => {
                let new_index = i + 1;
                self.history_index = Some(new_index);
                self.buffer = self.history[new_index].clone();
                self.cursor = self.buffer.len();
            }
        }
    }
}

impl Default for InputWidget {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn new_is_empty() {
        let w = InputWidget::new();
        assert_eq!(w.text(), "");
        assert_eq!(w.cursor, 0);
    }

    #[test]
    fn insert_char_at_cursor() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        assert_eq!(w.text(), "hi");
        assert_eq!(w.cursor, 2);

        // Insert in the middle.
        w.handle_key(key(KeyCode::Home));
        w.handle_key(key(KeyCode::Char('!')));
        assert_eq!(w.text(), "!hi");
        assert_eq!(w.cursor, 1);
    }

    #[test]
    fn backspace_removes_char() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(w.text(), "a");
        assert_eq!(w.cursor, 1);

        // Backspace at position 0 is a no-op.
        w.handle_key(key(KeyCode::Home));
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(w.text(), "a");
    }

    #[test]
    fn enter_submits_and_clears() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('h')));
        w.handle_key(key(KeyCode::Char('i')));
        let action = w.handle_key(key(KeyCode::Enter));
        assert_eq!(action, InputAction::Submit("hi".to_string()));
        assert_eq!(w.text(), "");
        assert_eq!(w.cursor, 0);
    }

    #[test]
    fn ctrl_c_returns_interrupt() {
        let mut w = InputWidget::new();
        let action = w.handle_key(ctrl('c'));
        assert_eq!(action, InputAction::Interrupt);
    }

    #[test]
    fn ctrl_d_empty_returns_exit() {
        let mut w = InputWidget::new();
        let action = w.handle_key(ctrl('d'));
        assert_eq!(action, InputAction::Exit);
    }

    #[test]
    fn ctrl_d_non_empty_deletes_char() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        // Move to start, then Ctrl+D should delete 'a'.
        w.handle_key(key(KeyCode::Home));
        let action = w.handle_key(ctrl('d'));
        assert_eq!(action, InputAction::None);
        assert_eq!(w.text(), "b");
    }

    #[test]
    fn cursor_movement_left_right() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.cursor, 2);

        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor, 1);

        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor, 0);

        // Left at 0 is a no-op.
        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor, 0);

        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor, 1);

        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor, 2);

        // Right at end is a no-op.
        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor, 2);
    }

    #[test]
    fn home_end_keys() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));

        w.handle_key(key(KeyCode::Home));
        assert_eq!(w.cursor, 0);

        w.handle_key(key(KeyCode::End));
        assert_eq!(w.cursor, 2);

        // Ctrl+A and Ctrl+E are aliases.
        w.handle_key(ctrl('a'));
        assert_eq!(w.cursor, 0);

        w.handle_key(ctrl('e'));
        assert_eq!(w.cursor, 2);
    }

    #[test]
    fn history_recall_up_down() {
        let mut w = InputWidget::new();
        w.push_history("first".to_string());
        w.push_history("second".to_string());

        // Type something new.
        w.handle_key(key(KeyCode::Char('x')));

        // Up → second (most recent).
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "second");

        // Up → first.
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "first");

        // Up at oldest stays at first.
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "first");

        // Down → second.
        w.handle_key(key(KeyCode::Down));
        assert_eq!(w.text(), "second");

        // Down → restore saved "x".
        w.handle_key(key(KeyCode::Down));
        assert_eq!(w.text(), "x");
        assert!(w.history_index.is_none());
    }

    #[test]
    fn paste_inserts_text() {
        let mut w = InputWidget::new();
        w.handle_paste("hello world");
        assert_eq!(w.text(), "hello world");
        assert_eq!(w.cursor, 11);
    }

    #[test]
    fn paste_converts_newlines_to_spaces() {
        let mut w = InputWidget::new();
        w.handle_paste("line1\nline2");
        assert_eq!(w.text(), "line1 line2");
    }

    #[test]
    fn ctrl_u_kills_to_start() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Char('c')));
        w.handle_key(key(KeyCode::Left)); // cursor at 2
        w.handle_key(ctrl('u'));
        assert_eq!(w.text(), "c");
        assert_eq!(w.cursor, 0);
    }

    #[test]
    fn ctrl_k_kills_to_end() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Char('c')));
        w.handle_key(key(KeyCode::Home));
        w.handle_key(key(KeyCode::Right)); // cursor at 1
        w.handle_key(ctrl('k'));
        assert_eq!(w.text(), "a");
        assert_eq!(w.cursor, 1);
    }

    #[test]
    fn delete_key_removes_char_at_cursor() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Home));
        w.handle_key(key(KeyCode::Delete));
        assert_eq!(w.text(), "b");
        assert_eq!(w.cursor, 0);
    }

    #[test]
    fn utf8_cursor_movement() {
        let mut w = InputWidget::new();
        // '€' is a 3-byte UTF-8 character.
        w.insert_char('€');
        w.insert_char('!');
        assert_eq!(w.text(), "€!");
        assert_eq!(w.cursor, 4); // 3 bytes + 1 byte

        w.move_cursor_left();
        assert_eq!(w.cursor, 3); // on '!'

        w.move_cursor_left();
        assert_eq!(w.cursor, 0); // on '€'

        w.move_cursor_right();
        assert_eq!(w.cursor, 3); // back to '!'
    }

    #[test]
    fn push_history_deduplicates_consecutive() {
        let mut w = InputWidget::new();
        w.push_history("same".to_string());
        w.push_history("same".to_string());
        assert_eq!(w.history.len(), 1);
    }

    #[test]
    fn push_history_ignores_empty() {
        let mut w = InputWidget::new();
        w.push_history(String::new());
        assert!(w.history.is_empty());
    }
}

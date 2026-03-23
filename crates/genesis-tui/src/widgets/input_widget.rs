//! Multi-line text input widget with readline-style editing and command history.
//!
//! Renders as `you> {text}` with a block cursor. Supports multi-line editing
//! via Shift+Enter (insert newline) and Enter (submit). The input area grows
//! dynamically to fit the content, up to a configurable maximum.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr as _;

use crate::history::rgb;

/// The prefix shown before user input (includes trailing space).
const PREFIX: &str = "you> ";

/// Width of the prefix in terminal columns.
const PREFIX_WIDTH: usize = 5;

/// Style for the `you> ` prefix.
const PREFIX_STYLE: Style = Style::new().fg(rgb(genesis_ui::colors::UI_DIM));

/// Style for normal (non-cursor) input text.
const TEXT_STYLE: Style = Style::new().fg(rgb(genesis_ui::colors::UI_TEXT));

/// Style for the character under the cursor.
const CURSOR_STYLE: Style = Style::new()
    .fg(rgb(genesis_ui::colors::UI_TEXT))
    .add_modifier(Modifier::REVERSED);

/// Style for the placeholder main text ("Ask Eve anything...").
const PLACEHOLDER_STYLE: Style = Style::new().fg(rgb(genesis_ui::colors::UI_DIM));

/// Style for the placeholder hint ("/ for commands").
const PLACEHOLDER_HINT_STYLE: Style = Style::new().fg(rgb(genesis_ui::colors::UI_MUTED));

/// Style for the multi-line hint shown below the placeholder.
const MULTILINE_HINT_STYLE: Style = Style::new().fg(rgb((80, 80, 80)));

/// Maximum number of visible rows for the input area (excluding border).
const MAX_INPUT_ROWS: u16 = 10;

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

/// Multi-line text input with readline-style key bindings and
/// command history navigation.
pub struct InputWidget {
    /// Current edit buffer (may contain `\n` for multi-line).
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

    /// Number of visual rows the content occupies (for dynamic layout).
    ///
    /// Accounts for both explicit newlines and visual wrapping when a
    /// logical line is wider than the available columns. The first line
    /// has fewer usable columns because of the `you> ` prefix; continuation
    /// lines are indented by the same prefix width.
    pub fn height(&self, width: u16) -> u16 {
        let usable = width.saturating_sub(PREFIX_WIDTH as u16).max(1) as usize;
        let mut rows: usize = 0;
        for logical_line in self.buffer.split('\n') {
            let line_w = logical_line.width();
            if line_w == 0 {
                rows += 1;
            } else {
                rows += (line_w.saturating_sub(1) / usable) + 1;
            }
        }
        (rows as u16).clamp(1, MAX_INPUT_ROWS)
    }

    /// Whether the buffer contains multiple lines.
    fn is_multiline(&self) -> bool {
        self.buffer.contains('\n')
    }

    // ── Event handling ────────────────────────────────────────────────────

    /// Handle a keyboard event and return the resulting [`InputAction`].
    ///
    /// ## Newline insertion
    ///
    /// Multiple keybindings are supported for inserting newlines, since
    /// Shift+Enter requires the Kitty keyboard protocol which many
    /// terminals don't support:
    ///
    /// - **Shift+Enter** — works in Kitty, WezTerm, foot, and other
    ///   terminals supporting the keyboard enhancement protocol
    /// - **Alt+Enter** (Option+Enter on macOS) — works in most terminals
    ///   when iTerm2 "Option sends Esc+" is enabled
    /// - **Ctrl+J** — universal fallback (ASCII newline, works everywhere)
    pub fn handle_key(&mut self, key: KeyEvent) -> InputAction {
        match (key.code, key.modifiers) {
            // ── Newline insertion (must be checked BEFORE plain Enter) ────
            // Shift+Enter — requires Kitty keyboard protocol
            (KeyCode::Enter, mods) if mods.contains(KeyModifiers::SHIFT) => {
                self.insert_char('\n');
                InputAction::None
            }
            // Alt+Enter — works in terminals with "Option sends Esc+"
            (KeyCode::Enter, mods) if mods.contains(KeyModifiers::ALT) => {
                self.insert_char('\n');
                InputAction::None
            }
            // Ctrl+J — universal ASCII newline, works in all terminals
            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                self.insert_char('\n');
                InputAction::None
            }

            // ── Submit (plain Enter) ─────────────────────────────────────
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
                // Kill from cursor to start of current line.
                let line_start = self.current_line_start();
                self.buffer.drain(line_start..self.cursor);
                self.cursor = line_start;
                InputAction::None
            }

            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                // Kill from cursor to end of current line.
                let line_end = self.current_line_end();
                self.buffer.drain(self.cursor..line_end);
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
                // Move to start of current line (not whole buffer).
                self.cursor = self.current_line_start();
                InputAction::None
            }

            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                // Move to end of current line (not whole buffer).
                self.cursor = self.current_line_end();
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

            // ── Up/Down: line navigation when multi-line, history when single-line
            (KeyCode::Up, _) => {
                if self.is_multiline() {
                    self.move_cursor_up();
                } else {
                    self.history_prev();
                }
                InputAction::None
            }

            (KeyCode::Down, _) => {
                if self.is_multiline() {
                    self.move_cursor_down();
                } else {
                    self.history_next();
                }
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
    /// Newlines inside the pasted text are preserved for multi-line editing.
    /// CRLF (`\r\n`) sequences are collapsed to a single `\n`.
    pub fn handle_paste(&mut self, text: &str) {
        // Normalize line endings: \r\n → \n, lone \r → \n.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in normalized.chars() {
            self.insert_char(c);
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Render the input line(s) into `buf` at `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        self.render_with_state(area, buf, true);
    }

    /// Render the input into `buf` with optional cursor visibility.
    pub fn render_with_state(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        // ── Placeholder when buffer is empty ─────────────────────────────
        if self.buffer.is_empty() {
            self.render_placeholder(area, buf, show_cursor);
            return;
        }

        let lines: Vec<&str> = self.buffer.split('\n').collect();
        let visible_lines = (area.height as usize).min(lines.len());

        // Determine which lines to show (scroll to keep cursor visible).
        let cursor_line = self.cursor_line_index();
        let scroll_offset = if cursor_line >= visible_lines {
            cursor_line - visible_lines + 1
        } else {
            0
        };

        for (row_idx, line_idx) in (scroll_offset..scroll_offset + visible_lines).enumerate() {
            let row = area.y + row_idx as u16;
            if row >= area.y + area.height {
                break;
            }

            let line_text = lines.get(line_idx).copied().unwrap_or("");
            let is_first_line = line_idx == 0;

            // Compute byte offsets for this line within the buffer.
            let line_byte_start = self.line_byte_offset(line_idx);
            let line_byte_end = line_byte_start + line_text.len();

            let mut x = area.x;

            // Prefix on first visible line only.
            if is_first_line {
                for ch in PREFIX.chars() {
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        let mut s = String::with_capacity(ch.len_utf8());
                        s.push(ch);
                        cell.set_symbol(&s);
                        cell.set_style(PREFIX_STYLE);
                    }
                    x += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
                }
            } else {
                // Indent continuation lines to align with content after prefix.
                for _ in 0..PREFIX_WIDTH {
                    if x >= area.x + area.width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        cell.set_symbol(" ");
                        cell.set_style(TEXT_STYLE);
                    }
                    x += 1;
                }
            }

            // Render the text content with cursor highlight.
            let cursor_in_this_line = self.cursor >= line_byte_start
                && self.cursor <= line_byte_end
                && line_idx == cursor_line;

            for (byte_offset, ch) in line_text.char_indices() {
                if x >= area.x + area.width {
                    break;
                }
                let abs_byte = line_byte_start + byte_offset;
                let is_cursor_pos = cursor_in_this_line && abs_byte == self.cursor;
                let style = if show_cursor && is_cursor_pos {
                    CURSOR_STYLE
                } else {
                    TEXT_STYLE
                };

                if let Some(cell) = buf.cell_mut((x, row)) {
                    let mut s = String::with_capacity(ch.len_utf8());
                    s.push(ch);
                    cell.set_symbol(&s);
                    cell.set_style(style);
                }
                x += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            }

            // Block cursor at end of line.
            if cursor_in_this_line && self.cursor == line_byte_end && x < area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_symbol(" ");
                    cell.set_style(if show_cursor {
                        CURSOR_STYLE
                    } else {
                        TEXT_STYLE
                    });
                }
            }
        }
    }

    // ── Placeholder rendering ──────────────────────────────────────────

    /// Render the placeholder text when the buffer is empty.
    ///
    /// Shows `you> Ask Eve anything... (/ for commands)` on the first row
    /// with the cursor at the prefix end, and `Shift+Enter for newline` as
    /// a dimmer hint on the second row (if there is room).
    fn render_placeholder(&self, area: Rect, buf: &mut Buffer, show_cursor: bool) {
        let row = area.y;
        let mut x = area.x;

        // Draw "you> " prefix.
        for ch in PREFIX.chars() {
            if x >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, row)) {
                let mut s = String::with_capacity(ch.len_utf8());
                s.push(ch);
                cell.set_symbol(&s);
                cell.set_style(PREFIX_STYLE);
            }
            x += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        }

        // Block cursor right after prefix.
        let cursor_x = x;
        if show_cursor && cursor_x < area.x + area.width {
            if let Some(cell) = buf.cell_mut((cursor_x, row)) {
                cell.set_symbol(" ");
                cell.set_style(CURSOR_STYLE);
            }
        }

        // Draw "Ask Eve anything... " in dim style (after the cursor position).
        let placeholder_main = "Ask Eve anything... ";
        let placeholder_hint = "(/ for commands)";
        // Start the placeholder text at cursor_x (overlapping the cursor char
        // is fine — the cursor block appears on top of the first placeholder char).
        let mut px = cursor_x;
        for ch in placeholder_main.chars() {
            if px >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((px, row)) {
                let mut s = String::with_capacity(ch.len_utf8());
                s.push(ch);
                cell.set_symbol(&s);
                // The first cell is the cursor; keep its style.
                if show_cursor && px == cursor_x {
                    cell.set_style(CURSOR_STYLE);
                } else {
                    cell.set_style(PLACEHOLDER_STYLE);
                }
            }
            px += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        }

        // Draw "(/ for commands)" in muted style.
        for ch in placeholder_hint.chars() {
            if px >= area.x + area.width {
                break;
            }
            if let Some(cell) = buf.cell_mut((px, row)) {
                let mut s = String::with_capacity(ch.len_utf8());
                s.push(ch);
                cell.set_symbol(&s);
                cell.set_style(PLACEHOLDER_HINT_STYLE);
            }
            px += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        }

        // Draw "Shift+Enter for newline" hint on the second row if space allows.
        if area.height >= 2 {
            let hint_row = area.y + 1;
            let hint_text = "Shift+Enter for newline";
            let mut hx = area.x + PREFIX_WIDTH as u16;
            for ch in hint_text.chars() {
                if hx >= area.x + area.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((hx, hint_row)) {
                    let mut s = String::with_capacity(ch.len_utf8());
                    s.push(ch);
                    cell.set_symbol(&s);
                    cell.set_style(MULTILINE_HINT_STYLE);
                }
                hx += 1;
            }
        }
    }

    // ── Line navigation helpers ──────────────────────────────────────────

    /// Returns the 0-based line index the cursor is on.
    fn cursor_line_index(&self) -> usize {
        self.buffer[..self.cursor].matches('\n').count()
    }

    /// Returns the byte offset of the start of line `line_idx`.
    fn line_byte_offset(&self, line_idx: usize) -> usize {
        if line_idx == 0 {
            return 0;
        }
        let mut count = 0;
        for (i, ch) in self.buffer.char_indices() {
            if ch == '\n' {
                count += 1;
                if count == line_idx {
                    return i + 1; // byte after the \n
                }
            }
        }
        self.buffer.len()
    }

    /// Returns the byte offset of the start of the current line.
    fn current_line_start(&self) -> usize {
        // Search backwards for \n from cursor.
        if self.cursor == 0 {
            return 0;
        }
        match self.buffer[..self.cursor].rfind('\n') {
            Some(pos) => pos + 1,
            None => 0,
        }
    }

    /// Returns the byte offset of the end of the current line (before \n or buffer end).
    fn current_line_end(&self) -> usize {
        match self.buffer[self.cursor..].find('\n') {
            Some(pos) => self.cursor + pos,
            None => self.buffer.len(),
        }
    }

    /// Column offset of cursor within the current line (in characters, not bytes).
    fn cursor_char_column(&self) -> usize {
        let line_start = self.current_line_start();
        self.buffer[line_start..self.cursor].chars().count()
    }

    /// Move cursor up one line, preserving character column position.
    fn move_cursor_up(&mut self) {
        let current_line = self.cursor_line_index();
        if current_line == 0 {
            return;
        }
        let col = self.cursor_char_column();
        let prev_line_start = self.line_byte_offset(current_line - 1);
        let prev_line_end = self.line_byte_offset(current_line) - 1; // before the \n
        let prev_line_text = &self.buffer[prev_line_start..prev_line_end];
        // Advance `col` characters into the previous line (clamped).
        let target_byte = prev_line_text
            .char_indices()
            .nth(col)
            .map(|(i, _)| prev_line_start + i)
            .unwrap_or(prev_line_end);
        self.cursor = target_byte;
    }

    /// Move cursor down one line, preserving character column position.
    fn move_cursor_down(&mut self) {
        let current_line = self.cursor_line_index();
        let total_lines = self.buffer.matches('\n').count() + 1;
        if current_line + 1 >= total_lines {
            return;
        }
        let col = self.cursor_char_column();
        let next_line_start = self.line_byte_offset(current_line + 1);
        let next_line_end = if current_line + 2 < total_lines {
            self.line_byte_offset(current_line + 2) - 1
        } else {
            self.buffer.len()
        };
        let next_line_text = &self.buffer[next_line_start..next_line_end];
        let target_byte = next_line_text
            .char_indices()
            .nth(col)
            .map(|(i, _)| next_line_start + i)
            .unwrap_or(next_line_end);
        self.cursor = target_byte;
    }

    // ── Private helpers ───────────────────────────────────────────────────

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
                self.saved_input = Some(self.buffer.clone());
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };

        self.history_index = Some(new_index);
        self.buffer = self.history[new_index].clone();
        self.cursor = self.buffer.len();
    }

    /// Navigate to the next (newer) history entry, or back to saved input.
    fn history_next(&mut self) {
        match self.history_index {
            None => {}
            Some(i) if i + 1 >= self.history.len() => {
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

    fn shift_enter() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)
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

        w.handle_key(key(KeyCode::Left));
        assert_eq!(w.cursor, 0);

        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor, 1);

        w.handle_key(key(KeyCode::Right));
        assert_eq!(w.cursor, 2);

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

        w.handle_key(key(KeyCode::Char('x')));

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "second");

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "first");

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "first");

        w.handle_key(key(KeyCode::Down));
        assert_eq!(w.text(), "second");

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
    fn paste_preserves_newlines() {
        let mut w = InputWidget::new();
        w.handle_paste("line1\nline2");
        assert_eq!(w.text(), "line1\nline2");
        assert!(w.is_multiline());
    }

    #[test]
    fn paste_normalizes_crlf() {
        let mut w = InputWidget::new();
        w.handle_paste("a\r\nb\r\nc");
        assert_eq!(w.text(), "a\nb\nc");
        // Should not double the newlines.
        assert_eq!(w.buffer.matches('\n').count(), 2);
    }

    #[test]
    fn ctrl_u_kills_to_start() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(key(KeyCode::Char('b')));
        w.handle_key(key(KeyCode::Char('c')));
        w.handle_key(key(KeyCode::Left));
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
        w.handle_key(key(KeyCode::Right));
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
        w.insert_char('€');
        w.insert_char('!');
        assert_eq!(w.text(), "€!");
        assert_eq!(w.cursor, 4);

        w.move_cursor_left();
        assert_eq!(w.cursor, 3);

        w.move_cursor_left();
        assert_eq!(w.cursor, 0);

        w.move_cursor_right();
        assert_eq!(w.cursor, 3);
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

    // ── Multi-line tests ─────────────────────────────────────────────────

    #[test]
    fn shift_enter_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        let action = w.handle_key(shift_enter());
        assert_eq!(action, InputAction::None);
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.text(), "a\nb");
        assert!(w.is_multiline());
    }

    #[test]
    fn alt_enter_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        let action = w.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(action, InputAction::None);
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.text(), "a\nb");
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        let action = w.handle_key(ctrl('j'));
        assert_eq!(action, InputAction::None);
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.text(), "a\nb");
    }

    #[test]
    fn enter_submits_multiline_text() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(shift_enter());
        w.handle_key(key(KeyCode::Char('b')));
        let action = w.handle_key(key(KeyCode::Enter));
        assert_eq!(action, InputAction::Submit("a\nb".to_string()));
    }

    #[test]
    fn height_reflects_line_count() {
        let mut w = InputWidget::new();
        assert_eq!(w.height(80), 1);

        w.handle_key(key(KeyCode::Char('a')));
        w.handle_key(shift_enter());
        w.handle_key(key(KeyCode::Char('b')));
        assert_eq!(w.height(80), 2);

        w.handle_key(shift_enter());
        w.handle_key(key(KeyCode::Char('c')));
        assert_eq!(w.height(80), 3);
    }

    #[test]
    fn up_down_navigate_lines_when_multiline() {
        let mut w = InputWidget::new();
        w.handle_paste("line1\nline2\nline3");
        // Cursor is at end of "line3".
        assert_eq!(w.cursor_line_index(), 2);

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.cursor_line_index(), 1);

        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.cursor_line_index(), 0);

        w.handle_key(key(KeyCode::Down));
        assert_eq!(w.cursor_line_index(), 1);
    }

    #[test]
    fn up_down_use_history_when_single_line() {
        let mut w = InputWidget::new();
        w.push_history("old".to_string());
        w.handle_key(key(KeyCode::Char('x')));

        // Single-line: Up should navigate history.
        w.handle_key(key(KeyCode::Up));
        assert_eq!(w.text(), "old");
    }

    #[test]
    fn home_end_work_per_line() {
        let mut w = InputWidget::new();
        w.handle_paste("abc\ndef");
        // Cursor at end of "def" (byte 7).
        assert_eq!(w.cursor, 7);

        // Home goes to start of "def" (byte 4, after \n).
        w.handle_key(key(KeyCode::Home));
        assert_eq!(w.cursor, 4);

        // End goes to end of "def" (byte 7).
        w.handle_key(key(KeyCode::End));
        assert_eq!(w.cursor, 7);
    }

    #[test]
    fn backspace_joins_lines() {
        let mut w = InputWidget::new();
        w.handle_paste("abc\ndef");
        // Move to start of "def".
        w.cursor = 4;
        // Backspace deletes the \n, joining lines.
        w.handle_key(key(KeyCode::Backspace));
        assert_eq!(w.text(), "abcdef");
    }

    #[test]
    fn ctrl_u_kills_to_line_start_not_buffer_start() {
        let mut w = InputWidget::new();
        w.handle_paste("abc\ndef");
        // Cursor at end of "def" (byte 7).
        w.handle_key(ctrl('u'));
        // Should kill "def" but leave "abc\n".
        assert_eq!(w.text(), "abc\n");
    }

    #[test]
    fn height_capped_at_max() {
        let mut w = InputWidget::new();
        // Create more than MAX_INPUT_ROWS lines.
        for i in 0..15 {
            if i > 0 {
                w.insert_char('\n');
            }
            w.insert_char('a');
        }
        assert_eq!(w.height(80), MAX_INPUT_ROWS);
    }

    // ── Placeholder tests ───────────────────────────────────────────────

    /// Helper: render the widget into a fresh buffer and return the raw cell symbols
    /// concatenated for each row (trimmed of trailing spaces).
    fn render_to_strings(w: &InputWidget, width: u16, height: u16) -> Vec<String> {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        w.render(area, &mut buf);
        (0..height)
            .map(|row| {
                let mut s = String::new();
                for col in 0..width {
                    if let Some(cell) = buf.cell((col, row)) {
                        s.push_str(cell.symbol());
                    }
                }
                s.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn placeholder_shown_when_empty() {
        let w = InputWidget::new();
        let rows = render_to_strings(&w, 60, 2);
        let first_line = &rows[0];
        // Must contain the prefix and placeholder text.
        assert!(
            first_line.contains("you>"),
            "expected 'you>' prefix, got: {first_line}"
        );
        assert!(
            first_line.contains("Ask Eve anything..."),
            "expected placeholder text, got: {first_line}"
        );
        assert!(
            first_line.contains("(/ for commands)"),
            "expected hint text, got: {first_line}"
        );
        // Second row should contain the multi-line hint.
        let second_line = &rows[1];
        assert!(
            second_line.contains("Shift+Enter for newline"),
            "expected multi-line hint, got: {second_line}"
        );
    }

    #[test]
    fn placeholder_hidden_when_buffer_has_content() {
        let mut w = InputWidget::new();
        w.handle_key(key(KeyCode::Char('x')));
        let rows = render_to_strings(&w, 60, 2);
        let first_line = &rows[0];
        assert!(
            !first_line.contains("Ask Eve anything..."),
            "placeholder should not appear when buffer has content, got: {first_line}"
        );
        // The typed character should appear.
        assert!(
            first_line.contains("x"),
            "expected typed char, got: {first_line}"
        );
    }

    // ── Visual wrapping height tests ──────────────────────────────────

    #[test]
    fn height_accounts_for_visual_wrapping() {
        let mut w = InputWidget::new();
        // With width=20 and prefix "you> " (5 cols), usable = 15.
        // A 30-char string with no newlines needs ceil(30/15) = 2 rows.
        w.handle_paste("aaaaabbbbbcccccdddddeeeeefffff");
        assert!(
            w.height(20) >= 2,
            "long single line should wrap; got height {}",
            w.height(20)
        );
    }

    #[test]
    fn height_single_short_line_is_one() {
        let mut w = InputWidget::new();
        w.handle_paste("hi");
        assert_eq!(w.height(80), 1);
    }

    #[test]
    fn height_combines_newlines_and_wrapping() {
        let mut w = InputWidget::new();
        // Two logical lines, first wraps at width 20 (usable 15).
        // "aaaaabbbbbccccc" (15 chars) + "ddddd" (5 chars) → 2 visual rows.
        // "short" → 1 visual row.
        // Total: 3 visual rows.
        w.handle_paste("aaaaabbbbbcccccddddd\nshort");
        assert!(
            w.height(20) >= 3,
            "expected at least 3 rows from wrapping + newline; got {}",
            w.height(20)
        );
    }
}

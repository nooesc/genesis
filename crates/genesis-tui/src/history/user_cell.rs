//! UserCell — renders the user's message with a dim `you> ` prefix.

use std::cell::Cell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr as _;

use super::{render_prefixed_lines, rgb};

const PREFIX: &str = "you> ";

/// A single user message cell.
#[derive(Debug, Clone)]
pub struct UserCell {
    /// The raw message text.
    pub text: String,
    /// Cached `(width, height)` to avoid recomputation when the width hasn't
    /// changed.  Uses interior mutability so `height()` can stay `&self`.
    cached_height: Cell<Option<(u16, u16)>>,
}

impl UserCell {
    /// Construct a new `UserCell` with the given message text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cached_height: Cell::new(None),
        }
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.to_scrollback_lines(area.width);
        let paragraph =
            ratatui::widgets::Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
        paragraph.render(area, buf);
    }

    /// Return the number of rows this cell occupies at the given terminal width.
    pub fn height(&self, width: u16) -> u16 {
        if let Some((cached_w, cached_h)) = self.cached_height.get() {
            if cached_w == width {
                return cached_h;
            }
        }
        let usable_width = width.max(1);
        let h = wrapped_row_count(&self.to_scrollback_lines(usable_width), usable_width).max(1);
        self.cached_height.set(Some((width, h)));
        h
    }

    /// Produce the styled [`Line`]s for scrollback insertion.
    pub fn to_scrollback_lines(&self, width: u16) -> Vec<Line<'static>> {
        render_prefixed_lines(
            &self.text,
            width,
            PREFIX,
            rgb(genesis_ui::colors::UI_DIM),
            rgb(genesis_ui::colors::UI_TEXT),
        )
    }
}

/// Simple word-wrap: split `text` into lines that fit within `max_width` columns.
/// Hard newlines (`\n`) are preserved as line breaks. Each resulting paragraph
/// is then wrapped on Unicode whitespace boundaries; individual words wider than
/// `max_width` are kept as-is. Returns at least one (possibly empty) element.
pub(crate) fn word_wrap(text: &str, max_width: u16) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }

    let mut result = Vec::new();
    for paragraph in text.split('\n') {
        let wrapped = wrap_single_line(paragraph, max_width);
        result.extend(wrapped);
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// Word-wrap a single paragraph (no embedded newlines) to fit within
/// `max_width` columns. Splits on any Unicode whitespace boundary; individual
/// words wider than `max_width` are kept as-is.
fn wrap_single_line(text: &str, max_width: u16) -> Vec<String> {
    let max = max_width as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    for word in text.split(char::is_whitespace) {
        let word_w = word.width();
        if current.is_empty() {
            current.push_str(word);
            current_width = word_w;
        } else {
            // +1 for the space separator
            if current_width + 1 + word_w <= max {
                current.push(' ');
                current.push_str(word);
                current_width += 1 + word_w;
            } else {
                lines.push(current.clone());
                current = word.to_string();
                current_width = word_w;
            }
        }
    }

    lines.push(current);
    lines
}

use super::wrapped_row_count;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_cell_height_short_message() {
        let cell = UserCell::new("hello");
        // "you> hello" fits in 80 cols → 1 line
        assert_eq!(cell.height(80), 1);
    }

    #[test]
    fn user_cell_height_wraps_long_message() {
        // Create a message that is definitely longer than 20 chars of text area.
        // prefix = "you> " = 5 chars, text area = 15.
        let text = "one two three four five six seven";
        let cell = UserCell::new(text);
        let h = cell.height(20);
        assert!(h >= 2, "expected wrapping; got height {h}");
    }

    #[test]
    fn user_cell_scrollback_lines_single_line() {
        let cell = UserCell::new("hello world");
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
        // First span should be the prefix.
        assert_eq!(lines[0].spans[0].content, PREFIX);
    }

    #[test]
    fn user_cell_scrollback_continuation_indented() {
        let text = "one two three four five six seven eight nine ten";
        let cell = UserCell::new(text);
        let lines = cell.to_scrollback_lines(20);
        assert!(lines.len() >= 2);
        // Continuation lines start with spaces (indent), not prefix text.
        let cont = &lines[1].spans[0].content;
        assert!(
            cont.chars().all(|c| c == ' '),
            "continuation line should start with spaces, got: {cont:?}"
        );
    }

    #[test]
    fn word_wrap_no_wrap_needed() {
        let result = word_wrap("hello world", 40);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn word_wrap_splits_on_boundary() {
        // max_width = 5: "hello" (5) fits, " world" (6) does not
        let result = word_wrap("hello world", 5);
        assert_eq!(result, vec!["hello", "world"]);
    }

    #[test]
    fn word_wrap_empty_string() {
        let result = word_wrap("", 40);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn word_wrap_splits_on_tab() {
        // Tab should be treated as a word boundary, not part of a word.
        let result = word_wrap("hello\tworld", 40);
        assert_eq!(result.len(), 1);
        // Both words are present (tab replaced by space as separator).
        assert!(result[0].contains("hello"));
        assert!(result[0].contains("world"));
    }

    #[test]
    fn word_wrap_splits_on_non_breaking_space() {
        // Non-breaking space (U+00A0) should also split.
        let result = word_wrap("hello\u{00A0}world", 6);
        assert_eq!(result.len(), 2, "expected 2 lines, got: {result:?}");
    }

    #[test]
    fn word_wrap_preserves_hard_newlines() {
        // Hard newlines should produce separate lines, not be collapsed.
        let result = word_wrap("line one\nline two\nline three", 40);
        assert_eq!(result.len(), 3, "expected 3 lines, got: {result:?}");
        assert_eq!(result[0], "line one");
        assert_eq!(result[1], "line two");
        assert_eq!(result[2], "line three");
    }

    #[test]
    fn word_wrap_hard_newline_with_wrapping() {
        // Hard newlines + word-wrapping within each paragraph.
        let result = word_wrap("hello world\nfoo bar baz", 6);
        // "hello world" wraps to ["hello", "world"], "foo bar baz" wraps to ["foo", "bar", "baz"]
        assert_eq!(result.len(), 5, "expected 5 lines, got: {result:?}");
        assert_eq!(result[0], "hello");
        assert_eq!(result[1], "world");
        assert_eq!(result[2], "foo");
        assert_eq!(result[3], "bar");
        assert_eq!(result[4], "baz");
    }

    #[test]
    fn word_wrap_trailing_newline() {
        // A trailing newline should produce an extra empty line.
        let result = word_wrap("hello\n", 40);
        assert_eq!(result.len(), 2, "expected 2 lines, got: {result:?}");
        assert_eq!(result[0], "hello");
        assert_eq!(result[1], "");
    }
}

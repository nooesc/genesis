//! UserCell — renders the user's message with a dim `you> ` prefix.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr as _;

/// Dim grey used for the `you> ` prefix.
const UI_DIM: Color = Color::Rgb(108, 108, 108);
/// Light grey used for message text.
const UI_TEXT: Color = Color::Rgb(208, 208, 208);

const PREFIX: &str = "you> ";

/// A single user message cell.
#[derive(Debug, Clone)]
pub struct UserCell {
    /// The raw message text.
    pub text: String,
}

impl UserCell {
    /// Construct a new `UserCell` with the given message text.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.to_scrollback_lines(area.width);
        let paragraph = ratatui::widgets::Paragraph::new(lines);
        paragraph.render(area, buf);
    }

    /// Return the number of rows this cell occupies at the given terminal width.
    pub fn height(&self, width: u16) -> u16 {
        self.to_scrollback_lines(width).len() as u16
    }

    /// Produce the styled [`Line`]s for scrollback insertion.
    pub fn to_scrollback_lines(&self, width: u16) -> Vec<Line<'static>> {
        let prefix_width = PREFIX.width() as u16;
        let text_width = width.saturating_sub(prefix_width);

        let wrapped = word_wrap(&self.text, text_width);

        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| {
                if i == 0 {
                    Line::from(vec![
                        Span::styled(PREFIX, Style::default().fg(UI_DIM)),
                        Span::styled(chunk, Style::default().fg(UI_TEXT)),
                    ])
                } else {
                    // Continuation lines: indent by prefix_width spaces.
                    let indent = " ".repeat(prefix_width as usize);
                    Line::from(vec![
                        Span::raw(indent),
                        Span::styled(chunk, Style::default().fg(UI_TEXT)),
                    ])
                }
            })
            .collect()
    }
}

/// Simple word-wrap: split `text` into lines that fit within `max_width` columns.
/// Splits on space boundaries; individual words wider than `max_width` are kept as-is.
/// Returns at least one (possibly empty) element.
pub(super) fn word_wrap(text: &str, max_width: u16) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }

    let max = max_width as usize;
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_width: usize = 0;

    for word in text.split(' ') {
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
}

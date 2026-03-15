//! AgentCell — renders Eve's response with a lavender `eve> ` prefix.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr as _;

use super::user_cell::word_wrap;

/// Lavender used for the `eve> ` prefix.
const EVE_LAVENDER: Color = Color::Rgb(180, 167, 214);
/// Light grey used for response text.
const UI_TEXT: Color = Color::Rgb(208, 208, 208);

const PREFIX: &str = "eve> ";

/// A single agent (Eve) response cell.
#[derive(Debug, Clone)]
pub struct AgentCell {
    /// The raw response text.
    pub text: String,
}

impl AgentCell {
    /// Construct a new `AgentCell` with the given response text.
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
                        Span::styled(PREFIX, Style::default().fg(EVE_LAVENDER)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_cell_height_short_message() {
        let cell = AgentCell::new("hello");
        // "eve> hello" fits in 80 cols → 1 line
        assert_eq!(cell.height(80), 1);
    }

    #[test]
    fn agent_cell_height_wraps_long_message() {
        let text = "one two three four five six seven";
        let cell = AgentCell::new(text);
        let h = cell.height(20);
        assert!(h >= 2, "expected wrapping; got height {h}");
    }

    #[test]
    fn agent_cell_scrollback_lines_single_line() {
        let cell = AgentCell::new("hello world");
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
        // First span should be the lavender prefix.
        assert_eq!(lines[0].spans[0].content, PREFIX);
        // Prefix style should use EVE_LAVENDER.
        assert_eq!(lines[0].spans[0].style.fg, Some(EVE_LAVENDER));
    }

    #[test]
    fn agent_cell_scrollback_continuation_indented() {
        let text = "one two three four five six seven eight nine ten";
        let cell = AgentCell::new(text);
        let lines = cell.to_scrollback_lines(20);
        assert!(lines.len() >= 2);
        let cont = &lines[1].spans[0].content;
        assert!(
            cont.chars().all(|c| c == ' '),
            "continuation line should start with spaces, got: {cont:?}"
        );
    }

    #[test]
    fn agent_cell_prefix_color_is_lavender() {
        let cell = AgentCell::new("hi");
        let lines = cell.to_scrollback_lines(80);
        let prefix_span = &lines[0].spans[0];
        assert_eq!(
            prefix_span.style.fg,
            Some(Color::Rgb(180, 167, 214)),
            "prefix should be EVE_LAVENDER"
        );
    }
}

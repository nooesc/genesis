//! AgentCell — renders Eve's response with a lavender `eve> ` prefix.
//!
//! The response text is parsed as markdown so that headers, bold, italic,
//! inline code, code fences, and lists are rendered with appropriate ratatui
//! styles.  The `eve> ` prefix is prepended to the first line and
//! continuation lines are indented by the same width.

use std::cell::OnceCell;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr as _;

use super::rgb;
use crate::render::markdown::markdown_to_lines;

const PREFIX: &str = "eve> ";

/// A single agent (Eve) response cell.
#[derive(Debug, Clone)]
pub struct AgentCell {
    /// The raw response text.
    pub text: String,
    /// Lazily-computed styled lines (avoids re-parsing markdown in both
    /// `height()` and `to_scrollback_lines()`).
    cached_lines: OnceCell<Vec<Line<'static>>>,
}

impl AgentCell {
    /// Construct a new `AgentCell` with the given response text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            cached_lines: OnceCell::new(),
        }
    }

    /// Return the cached (or lazily computed) styled lines.
    fn lines(&self) -> &Vec<Line<'static>> {
        self.cached_lines
            .get_or_init(|| prefix_markdown_lines(&self.text))
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.to_scrollback_lines(area.width);
        let paragraph = ratatui::widgets::Paragraph::new(lines);
        paragraph.render(area, buf);
    }

    /// Return the number of rows this cell occupies at the given terminal width.
    pub fn height(&self, width: u16) -> u16 {
        let usable_width = width.max(1);
        wrapped_row_count(&self.to_scrollback_lines(usable_width), usable_width).max(1)
    }

    /// Produce the styled [`Line`]s for scrollback insertion.
    ///
    /// The markdown renderer handles headers, bold, italic, code blocks, and
    /// lists.  We prepend the coloured `eve> ` prefix to the first line and
    /// a matching-width indent to every subsequent line.
    pub fn to_scrollback_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.lines().clone()
    }
}

/// Parse `text` as markdown and prepend the `eve> ` prefix/indent.
pub(crate) fn prefix_markdown_lines(text: &str) -> Vec<Line<'static>> {
    let md_lines = markdown_to_lines(text);

    if md_lines.is_empty() {
        // Even for empty text, produce one line with just the prefix so the
        // cell is visible.
        return vec![Line::from(Span::styled(
            PREFIX.to_owned(),
            Style::default().fg(rgb(genesis_ui::colors::EVE_LAVENDER)),
        ))];
    }

    let prefix_width = PREFIX.width();
    let indent: String = " ".repeat(prefix_width);
    let prefix_style = Style::default().fg(rgb(genesis_ui::colors::EVE_LAVENDER));

    md_lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let mut spans = Vec::with_capacity(1 + line.spans.len());
            if i == 0 {
                spans.push(Span::styled(PREFIX.to_owned(), prefix_style));
            } else {
                spans.push(Span::raw(indent.clone()));
            }
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

fn wrapped_row_count(lines: &[Line<'_>], wrap_width: u16) -> u16 {
    let width = wrap_width.max(1) as usize;
    let mut rows: usize = 0;
    for line in lines {
        let line_width = line
            .spans
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>();
        let wrapped = if line_width == 0 {
            1
        } else {
            (line_width.saturating_sub(1) / width) + 1
        };
        rows = rows.saturating_add(wrapped);
    }
    rows.try_into().unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn agent_cell_height_short_message() {
        let cell = AgentCell::new("hello");
        // Plain text → 1 markdown line → height 1.
        assert_eq!(cell.height(80), 1);
    }

    #[test]
    fn agent_cell_height_multiline_markdown() {
        // Two source lines → two markdown lines → height 2.
        let text = "first line\nsecond line";
        let cell = AgentCell::new(text);
        assert_eq!(cell.height(80), 2);
    }

    #[test]
    fn agent_cell_scrollback_lines_single_line() {
        let cell = AgentCell::new("hello world");
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
        // First span should be the lavender prefix.
        assert_eq!(lines[0].spans[0].content, PREFIX);
        // Prefix style should use EVE_LAVENDER.
        assert_eq!(
            lines[0].spans[0].style.fg,
            Some(rgb(genesis_ui::colors::EVE_LAVENDER))
        );
    }

    #[test]
    fn agent_cell_scrollback_continuation_indented() {
        let text = "line one\nline two";
        let cell = AgentCell::new(text);
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 2);
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

    #[test]
    fn agent_cell_renders_markdown() {
        let text = "# Heading\n\nSome **bold** and `code`.";
        let cell = AgentCell::new(text);
        let lines = cell.to_scrollback_lines(80);
        // 3 source lines → 3 ratatui lines.
        assert_eq!(lines.len(), 3);

        // First line: prefix + heading spans (bold + accent colour).
        assert_eq!(lines[0].spans[0].content, PREFIX);
        let heading_span = &lines[0].spans[1];
        assert_eq!(heading_span.content, "Heading");
        assert!(heading_span.style.add_modifier.contains(Modifier::BOLD));

        // Third line has bold and code spans (after the indent).
        let third = &lines[2];
        assert_eq!(third.spans[0].content, "     "); // indent = PREFIX width
        // Find the bold span.
        let bold = third.spans.iter().find(|s| s.content == "bold");
        assert!(bold.is_some(), "expected bold span in third line");
        assert!(bold.unwrap().style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn agent_cell_empty_text_produces_prefix_only() {
        let cell = AgentCell::new("");
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, PREFIX);
    }
}

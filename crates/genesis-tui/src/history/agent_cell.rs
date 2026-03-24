//! AgentCell — renders Eve's response with a lavender `eve> ` prefix.
//!
//! The response text is parsed as markdown so that headers, bold, italic,
//! inline code, code fences, and lists are rendered with appropriate ratatui
//! styles.  The `eve> ` prefix is prepended to the first line and
//! continuation lines are indented by the same width.

use std::cell::{Cell, OnceCell};

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
#[derive(Debug)]
pub struct AgentCell {
    /// The raw response text (private — use `text()` accessor).
    text: String,
    /// When true, this cell is a continuation of a prior agent block in the
    /// same turn — rendered with indent instead of the `eve> ` prefix.
    continuation: bool,
    /// Lazily-computed styled lines (avoids re-parsing markdown in both
    /// `height()` and `to_scrollback_lines()`).
    cached_lines: OnceCell<Vec<Line<'static>>>,
    /// Cached `(width, height)` to avoid recomputing `wrapped_row_count` when
    /// the width hasn't changed.  Uses interior mutability so `height()` can
    /// stay `&self`.
    cached_height: Cell<Option<(u16, u16)>>,
}

impl Clone for AgentCell {
    fn clone(&self) -> Self {
        let cached_lines = OnceCell::new();
        // Preserve the cached lines from the source to avoid re-parsing markdown.
        if let Some(lines) = self.cached_lines.get() {
            let _ = cached_lines.set(lines.clone());
        }
        Self {
            text: self.text.clone(),
            continuation: self.continuation,
            cached_lines,
            cached_height: self.cached_height.clone(),
        }
    }
}

impl AgentCell {
    /// Construct a new `AgentCell` with the given response text.
    ///
    /// This is the first block of an agent turn — rendered with the `eve> ` prefix.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            continuation: false,
            cached_lines: OnceCell::new(),
            cached_height: Cell::new(None),
        }
    }

    /// Construct a continuation `AgentCell` — a subsequent block in the same turn.
    ///
    /// Rendered with matching-width indent instead of the `eve> ` prefix.
    pub fn new_continuation(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            continuation: true,
            cached_lines: OnceCell::new(),
            cached_height: Cell::new(None),
        }
    }

    /// The raw response text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the cached (or lazily computed) styled lines.
    fn lines(&self) -> &Vec<Line<'static>> {
        self.cached_lines.get_or_init(|| {
            if self.continuation {
                continuation_markdown_lines(&self.text)
            } else {
                prefix_markdown_lines(&self.text)
            }
        })
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
        let lines = self.to_scrollback_lines(usable_width);
        // Empty continuation cells (no visible lines) should occupy 0 rows,
        // but cells with the eve> prefix always occupy at least 1 row.
        let h = if lines.is_empty() {
            0
        } else {
            wrapped_row_count(&lines, usable_width).max(1)
        };
        self.cached_height.set(Some((width, h)));
        h
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
///
/// The first line gets the coloured `eve> ` prefix; continuation lines
/// get a matching-width space indent.
pub(crate) fn prefix_markdown_lines(text: &str) -> Vec<Line<'static>> {
    format_markdown_lines_inner(text, false)
}

/// Parse `text` as markdown with indent only (no `eve> ` prefix).
///
/// Used for continuation blocks within the same agent turn — all lines
/// get the matching-width space indent so they align with the first block.
pub(crate) fn continuation_markdown_lines(text: &str) -> Vec<Line<'static>> {
    format_markdown_lines_inner(text, true)
}

/// Shared implementation: render markdown with either `eve> ` prefix or
/// indent-only on the first line.
fn format_markdown_lines_inner(text: &str, continuation: bool) -> Vec<Line<'static>> {
    let md_lines = markdown_to_lines(text);

    if md_lines.is_empty() {
        if continuation {
            // Continuation of empty text — produce nothing visible.
            return vec![];
        }
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
            if i == 0 && !continuation {
                spans.push(Span::styled(PREFIX.to_owned(), prefix_style));
            } else {
                spans.push(Span::raw(indent.clone()));
            }
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect()
}

use super::wrapped_row_count;

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
        // Heading + paragraph text (pulldown-cmark does not emit blank lines
        // between heading and following paragraph).
        assert!(
            lines.len() >= 2,
            "should have at least heading + text, got {}",
            lines.len()
        );

        // First line: prefix + heading spans (bold + accent colour).
        assert_eq!(lines[0].spans[0].content, PREFIX);
        let heading_span = &lines[0].spans[1];
        assert_eq!(heading_span.content, "Heading");
        assert!(heading_span.style.add_modifier.contains(Modifier::BOLD));

        // Last line has bold and code spans (after the indent).
        let last = lines.last().unwrap();
        assert_eq!(last.spans[0].content, "     "); // indent = PREFIX width
                                                    // Find the bold span.
        let bold = last.spans.iter().find(|s| s.content == "bold");
        assert!(bold.is_some(), "expected bold span in last line");
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

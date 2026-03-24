//! Conversation history cells.
//!
//! Each cell represents one displayable unit in the conversation: a user
//! message, an agent response, or a tool invocation.

pub mod agent_cell;
pub mod cell;
pub mod tool_cell;
pub mod user_cell;

pub use agent_cell::AgentCell;
pub use cell::HistoryCell;
pub use tool_cell::ToolCell;
pub use user_cell::UserCell;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr as _;

use user_cell::word_wrap;

/// Convert a genesis-ui `(u8, u8, u8)` colour tuple to a ratatui [`Color`].
pub(crate) const fn rgb(c: (u8, u8, u8)) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Count the total rows these lines would occupy when wrapped at `wrap_width`.
///
/// Simulates word-boundary wrapping (matching ratatui `Paragraph::wrap`) by
/// splitting each line on whitespace and tracking how words fill each row.
/// Words wider than the wrap width are placed on their own row (matching
/// ratatui behaviour). This produces a much closer estimate than simple
/// character-width division.
pub(crate) fn wrapped_row_count(lines: &[Line<'_>], wrap_width: u16) -> u16 {
    let max_w = wrap_width.max(1) as usize;
    let mut rows: usize = 0;

    for line in lines {
        // Collect the full text of the line for word splitting.
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if text.is_empty() {
            rows += 1;
            continue;
        }

        // Walk words and simulate how ratatui fills rows.
        let mut col: usize = 0;
        let mut line_rows: usize = 1;
        for word in text.split(char::is_whitespace) {
            let w = word.width();
            if col == 0 {
                // First word on a row — always placed (even if oversized).
                col = w;
            } else if col + 1 + w <= max_w {
                // Fits on current row with a space separator.
                col += 1 + w;
            } else {
                // Doesn't fit — start a new row.
                line_rows += 1;
                col = w;
            }
            // If the word itself is wider than max_w it may span multiple rows.
            // Account for the extra rows beyond the first.
            if w > max_w {
                let extra = w.saturating_sub(1) / max_w;
                line_rows += extra;
                col = w - extra * max_w;
            }
        }
        rows = rows.saturating_add(line_rows);
    }

    rows.try_into().unwrap_or(u16::MAX)
}

/// Like [`wrapped_row_count`] but returns `usize` to avoid saturation at
/// `u16::MAX`. Needed by the transcript overlay where total line count can
/// exceed 65 535 in long conversations.
pub(crate) fn wrapped_row_count_usize(lines: &[Line<'_>], wrap_width: u16) -> usize {
    let max_w = wrap_width.max(1) as usize;
    let mut rows: usize = 0;

    for line in lines {
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        if text.is_empty() {
            rows += 1;
            continue;
        }
        let mut col: usize = 0;
        let mut line_rows: usize = 1;
        for word in text.split(char::is_whitespace) {
            let w = word.width();
            if col == 0 {
                col = w;
            } else if col + 1 + w <= max_w {
                col += 1 + w;
            } else {
                line_rows += 1;
                col = w;
            }
            if w > max_w {
                let extra = w.saturating_sub(1) / max_w;
                line_rows += extra;
                col = w - extra * max_w;
            }
        }
        rows += line_rows;
    }
    rows
}

/// Render text with a coloured prefix on the first line and indent on
/// continuation lines after word-wrapping.
///
/// The indent string is computed once from the display width of `prefix` and
/// reused for every continuation line.
pub(crate) fn render_prefixed_lines(
    text: &str,
    width: u16,
    prefix: &str,
    prefix_color: Color,
    text_color: Color,
) -> Vec<Line<'static>> {
    let prefix_width = prefix.width() as u16;
    let text_width = width.saturating_sub(prefix_width);

    let wrapped = word_wrap(text, text_width);
    let indent: String = " ".repeat(prefix_width as usize);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            if i == 0 {
                Line::from(vec![
                    Span::styled(prefix.to_owned(), Style::default().fg(prefix_color)),
                    Span::styled(chunk, Style::default().fg(text_color)),
                ])
            } else {
                Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(chunk, Style::default().fg(text_color)),
                ])
            }
        })
        .collect()
}

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

//! Tool approval overlay — shows pending tool calls for user approval/denial.
//!
//! Displayed as a centered modal when a tool with `ApprovalPolicy::Destructive`
//! or `ApprovalPolicy::Always` is called. The user presses `y` to approve or
//! `n`/`Esc` to deny.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget as _},
};

use crate::history::rgb;

const ACCENT: Color = rgb(genesis_ui::colors::EVE_LAVENDER);
const TEXT: Color = rgb(genesis_ui::colors::UI_TEXT);
const DIM: Color = rgb(genesis_ui::colors::UI_DIM);
const WARNING: Color = rgb(genesis_ui::colors::UI_WARNING);
const BORDER: Color = Color::Rgb(88, 88, 88);

/// Result of handling a key in the approval overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    /// No action — key consumed.
    None,
    /// User approved the tool call.
    Approve,
    /// User denied the tool call.
    Deny,
}

/// Overlay widget showing a pending tool approval request.
pub struct ApprovalOverlay {
    /// Tool name being requested.
    tool_name: String,
    /// Pre-rendered argument lines.
    arg_lines: Vec<(String, String)>,
    /// Scroll offset for long argument lists.
    scroll: usize,
}

impl ApprovalOverlay {
    /// Create a new approval overlay for the given tool call.
    pub fn new(tool_name: String, arguments: &std::collections::BTreeMap<String, String>) -> Self {
        let arg_lines: Vec<(String, String)> = arguments
            .iter()
            .map(|(k, v)| {
                // Truncate long values for display.
                let display = if v.len() > 200 {
                    format!("{}…", &v[..v.floor_char_boundary(200)])
                } else {
                    v.clone()
                };
                (k.clone(), display)
            })
            .collect();

        Self {
            tool_name,
            arg_lines,
            scroll: 0,
        }
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> ApprovalAction {
        match (key.code, key.modifiers) {
            // Approve
            (KeyCode::Char('y'), _) | (KeyCode::Enter, _) => ApprovalAction::Approve,
            // Deny
            (KeyCode::Char('n'), _) | (KeyCode::Esc, _) => ApprovalAction::Deny,
            // Ctrl+C denies
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => ApprovalAction::Deny,
            // Scroll
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                self.scroll = self.scroll.saturating_add(1);
                ApprovalAction::None
            }
            (KeyCode::Up | KeyCode::Char('k'), _) => {
                self.scroll = self.scroll.saturating_sub(1);
                ApprovalAction::None
            }
            _ => ApprovalAction::None,
        }
    }

    /// Render the approval overlay as a centered modal.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 20 || area.height < 6 {
            return;
        }

        // Modal dimensions: 70% width, up to 60% height.
        let modal_width = (area.width * 7 / 10).clamp(30, 80);
        let content_lines = self.build_content_lines(modal_width);
        let modal_height = (content_lines.len() as u16 + 4) // +4 for borders + header + footer
            .min(area.height * 6 / 10)
            .max(6);

        let modal_x = area.x + (area.width.saturating_sub(modal_width)) / 2;
        let modal_y = area.y + (area.height.saturating_sub(modal_height)) / 2;

        let modal_area = Rect {
            x: modal_x,
            y: modal_y,
            width: modal_width,
            height: modal_height,
        };

        // Draw background (dim existing content).
        self.draw_border(modal_area, buf);

        // Header.
        if modal_area.height > 2 {
            let header = Line::from(vec![
                Span::styled(" ⚠ ", Style::default().fg(WARNING).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!("Approve tool: {}", self.tool_name),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
            ]);
            let header_area = Rect {
                x: modal_area.x + 1,
                y: modal_area.y + 1,
                width: modal_area.width.saturating_sub(2),
                height: 1,
            };
            Paragraph::new(header).render(header_area, buf);
        }

        // Content (arguments).
        if modal_area.height > 4 {
            let content_area = Rect {
                x: modal_area.x + 2,
                y: modal_area.y + 3,
                width: modal_area.width.saturating_sub(4),
                height: modal_area.height.saturating_sub(5),
            };
            let visible = content_area.height as usize;
            let end = (self.scroll + visible).min(content_lines.len());
            let start = end.saturating_sub(visible);
            let visible_lines: Vec<Line<'_>> = content_lines[start..end].to_vec();
            Paragraph::new(visible_lines).render(content_area, buf);
        }

        // Footer with key hints.
        let footer_y = modal_area.y + modal_area.height - 1;
        if footer_y > modal_area.y {
            let footer = Line::from(vec![
                Span::styled(" y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(" approve  ", Style::default().fg(DIM)),
                Span::styled("n", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(" deny  ", Style::default().fg(DIM)),
                Span::styled("Esc", Style::default().fg(DIM)),
                Span::styled(" cancel", Style::default().fg(DIM)),
            ]);
            let footer_area = Rect {
                x: modal_area.x + 1,
                y: footer_y,
                width: modal_area.width.saturating_sub(2),
                height: 1,
            };
            Paragraph::new(footer).render(footer_area, buf);
        }
    }

    fn build_content_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for (key, value) in &self.arg_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key}: "),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(value.clone(), Style::default().fg(DIM)),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no arguments)",
                Style::default().fg(DIM),
            )));
        }
        lines
    }

    fn draw_border(&self, area: Rect, buf: &mut Buffer) {
        let border_style = Style::default().fg(BORDER);
        let bg_style = Style::default().bg(Color::Rgb(25, 25, 30));

        // Fill background.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(" ");
                    cell.set_style(bg_style);
                }
            }
        }

        // Top border.
        if let Some(cell) = buf.cell_mut((area.x, area.y)) {
            cell.set_symbol("┌"); cell.set_style(border_style);
        }
        for x in area.x + 1..area.x + area.width - 1 {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_symbol("─"); cell.set_style(border_style);
            }
        }
        if let Some(cell) = buf.cell_mut((area.x + area.width - 1, area.y)) {
            cell.set_symbol("┐"); cell.set_style(border_style);
        }

        // Bottom border.
        let bottom = area.y + area.height - 1;
        if let Some(cell) = buf.cell_mut((area.x, bottom)) {
            cell.set_symbol("└"); cell.set_style(border_style);
        }
        for x in area.x + 1..area.x + area.width - 1 {
            if let Some(cell) = buf.cell_mut((x, bottom)) {
                cell.set_symbol("─"); cell.set_style(border_style);
            }
        }
        if let Some(cell) = buf.cell_mut((area.x + area.width - 1, bottom)) {
            cell.set_symbol("┘"); cell.set_style(border_style);
        }

        // Side borders.
        for y in area.y + 1..bottom {
            if let Some(cell) = buf.cell_mut((area.x, y)) {
                cell.set_symbol("│"); cell.set_style(border_style);
            }
            if let Some(cell) = buf.cell_mut((area.x + area.width - 1, y)) {
                cell.set_symbol("│"); cell.set_style(border_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn approve_on_y() {
        let mut overlay = ApprovalOverlay::new("shell_exec".to_owned(), &BTreeMap::new());
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert_eq!(action, ApprovalAction::Approve);
    }

    #[test]
    fn deny_on_n() {
        let mut overlay = ApprovalOverlay::new("shell_exec".to_owned(), &BTreeMap::new());
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(action, ApprovalAction::Deny);
    }

    #[test]
    fn deny_on_esc() {
        let mut overlay = ApprovalOverlay::new("shell_exec".to_owned(), &BTreeMap::new());
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, ApprovalAction::Deny);
    }

    #[test]
    fn approve_on_enter() {
        let mut overlay = ApprovalOverlay::new("shell_exec".to_owned(), &BTreeMap::new());
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ApprovalAction::Approve);
    }

    #[test]
    fn shows_arguments() {
        let args = BTreeMap::from([
            ("command".to_owned(), "rm -rf /".to_owned()),
        ]);
        let overlay = ApprovalOverlay::new("shell_exec".to_owned(), &args);
        assert_eq!(overlay.arg_lines.len(), 1);
        assert_eq!(overlay.arg_lines[0].0, "command");
    }
}

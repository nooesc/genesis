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
const SHELL_COLOR: Color = Color::Rgb(215, 95, 95);
const WRITE_COLOR: Color = Color::Rgb(212, 165, 116);
const ADD_COLOR: Color = Color::Rgb(135, 175, 95);
const DEL_COLOR: Color = Color::Rgb(215, 95, 95);

/// Result of handling a key in the approval overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    /// No action — key consumed.
    None,
    /// User approved the tool call.
    Approve,
    /// User denied the tool call.
    Deny,
    /// Approve this tool and remember approval for the session.
    AlwaysApprove,
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
                let display = if k == "old_text" || k == "new_text" || k == "content" {
                    // Keep more content for diff preview (up to 2000 chars).
                    if v.len() > 2000 {
                        format!("{}…", &v[..v.floor_char_boundary(2000)])
                    } else {
                        v.clone()
                    }
                } else if v.len() > 200 {
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

    /// The name of the tool being requested.
    pub fn tool_name(&self) -> &str {
        &self.tool_name
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> ApprovalAction {
        match (key.code, key.modifiers) {
            // Approve
            (KeyCode::Char('y'), _) | (KeyCode::Enter, _) => ApprovalAction::Approve,
            // Always approve this tool for the session
            (KeyCode::Char('a'), _) => ApprovalAction::AlwaysApprove,
            // Deny
            (KeyCode::Char('n'), _) | (KeyCode::Esc, _) => ApprovalAction::Deny,
            // Ctrl+C denies
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => ApprovalAction::Deny,
            // Scroll
            (KeyCode::Down | KeyCode::Char('j'), _) => {
                let content_len = self.build_content_lines(80).len();
                let max = content_len.saturating_sub(1);
                self.scroll = self.scroll.saturating_add(1).min(max);
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

        // Draw shadow behind the modal for depth.
        crate::render::render_shadow(modal_area, buf);

        // Draw background (dim existing content).
        self.draw_border(modal_area, buf);

        // Header.
        if modal_area.height > 2 {
            let tool_color = match self.tool_name.as_str() {
                "shell_exec" | "docker_exec" | "ssh_exec" | "code_execute" => SHELL_COLOR,
                "write_file" | "patch_file" | "create_file" | "delete_file" => WRITE_COLOR,
                _ if self.tool_name.starts_with("git_") => WRITE_COLOR,
                _ => ACCENT,
            };
            let header = Line::from(vec![
                Span::styled(" ⚠ ", Style::default().fg(WARNING).add_modifier(Modifier::BOLD)),
                Span::styled("Approve tool: ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(
                    self.tool_name.clone(),
                    Style::default().fg(tool_color).add_modifier(Modifier::BOLD),
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
                Span::styled("a", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(" always  ", Style::default().fg(DIM)),
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

    pub(crate) fn build_content_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Shell tools: highlight command prominently.
        let is_shell = matches!(
            self.tool_name.as_str(),
            "shell_exec" | "docker_exec" | "ssh_exec" | "code_execute"
        );

        if is_shell {
            if let Some((_, cmd)) = self
                .arg_lines
                .iter()
                .find(|(k, _)| k == "command" || k == "cmd")
            {
                lines.push(Line::from(vec![
                    Span::styled(
                        "  $ ",
                        Style::default()
                            .fg(SHELL_COLOR)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(cmd.clone(), Style::default().fg(TEXT)),
                ]));
                // Show other args below if any.
                for (key, value) in &self.arg_lines {
                    if key != "command" && key != "cmd" {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {key}: "),
                                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(value.clone(), Style::default().fg(DIM)),
                        ]));
                    }
                }
                return lines_or_empty(lines);
            }
        }

        // File edit tools: show diff preview if content includes old/new text.
        let is_file_edit = matches!(
            self.tool_name.as_str(),
            "write_file" | "patch_file" | "create_file" | "delete_file"
        );

        if is_file_edit {
            // Show path prominently.
            if let Some((_, path)) = self.arg_lines.iter().find(|(k, _)| k == "path") {
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        path.clone(),
                        Style::default()
                            .fg(WRITE_COLOR)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ]));
            }
            // Show old_text/new_text as a mini diff.
            let old_text = self
                .arg_lines
                .iter()
                .find(|(k, _)| k == "old_text")
                .map(|(_, v)| v.as_str());
            let new_text = self
                .arg_lines
                .iter()
                .find(|(k, _)| k == "new_text")
                .map(|(_, v)| v.as_str());
            if let (Some(old), Some(new)) = (old_text, new_text) {
                lines.push(Line::default()); // spacer
                for line in old.lines().take(10) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  - ",
                            Style::default()
                                .fg(DEL_COLOR)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(line.to_owned(), Style::default().fg(DEL_COLOR)),
                    ]));
                }
                for line in new.lines().take(10) {
                    lines.push(Line::from(vec![
                        Span::styled(
                            "  + ",
                            Style::default()
                                .fg(ADD_COLOR)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(line.to_owned(), Style::default().fg(ADD_COLOR)),
                    ]));
                }
                if old.lines().count() > 10 || new.lines().count() > 10 {
                    lines.push(Line::from(Span::styled(
                        "  ...(truncated)",
                        Style::default().fg(DIM),
                    )));
                }
                // Show remaining non-path/old/new args.
                for (key, value) in &self.arg_lines {
                    if key != "path" && key != "old_text" && key != "new_text" && key != "content" {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("  {key}: "),
                                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(value.clone(), Style::default().fg(DIM)),
                        ]));
                    }
                }
                return lines_or_empty(lines);
            }
            // write_file with content: show content preview.
            if let Some((_, content)) = self.arg_lines.iter().find(|(k, _)| k == "content") {
                let preview_lines: Vec<&str> = content.lines().take(10).collect();
                lines.push(Line::default());
                for line in &preview_lines {
                    lines.push(Line::from(vec![
                        Span::styled("  ", Style::default()),
                        Span::styled((*line).to_owned(), Style::default().fg(DIM)),
                    ]));
                }
                if content.lines().count() > 10 {
                    lines.push(Line::from(Span::styled(
                        "  ...(truncated)",
                        Style::default().fg(DIM),
                    )));
                }
                return lines_or_empty(lines);
            }

            // File edit tool without diff or content (e.g. delete_file with only path).
            // Show remaining non-path args and return without falling through to generic.
            for (key, value) in &self.arg_lines {
                if key != "path" {
                    lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {key}: "),
                            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(value.clone(), Style::default().fg(DIM)),
                    ]));
                }
            }
            return lines_or_empty(lines);
        }

        // Default: generic key-value rendering.
        for (key, value) in &self.arg_lines {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {key}: "),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(value.clone(), Style::default().fg(DIM)),
            ]));
        }

        lines_or_empty(lines)
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

fn lines_or_empty(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    if lines.is_empty() {
        vec![Line::from(Span::styled(
            "  (no arguments)",
            Style::default().fg(DIM),
        ))]
    } else {
        lines
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

    #[test]
    fn always_approve_on_a() {
        let mut overlay = ApprovalOverlay::new("shell_exec".to_owned(), &BTreeMap::new());
        let action = overlay.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        assert_eq!(action, ApprovalAction::AlwaysApprove);
    }

    #[test]
    fn shell_shows_dollar_prompt() {
        let args = BTreeMap::from([("command".to_owned(), "ls -la".to_owned())]);
        let overlay = ApprovalOverlay::new("shell_exec".to_owned(), &args);
        let lines = overlay.build_content_lines(80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("$ "), "shell should show $ prompt");
        assert!(text.contains("ls -la"), "shell should show command");
    }

    #[test]
    fn patch_shows_diff_preview() {
        let args = BTreeMap::from([
            ("path".to_owned(), "src/main.rs".to_owned()),
            ("old_text".to_owned(), "fn old() {}".to_owned()),
            ("new_text".to_owned(), "fn new() {}".to_owned()),
        ]);
        let overlay = ApprovalOverlay::new("patch_file".to_owned(), &args);
        let lines = overlay.build_content_lines(80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("src/main.rs"), "should show path");
        assert!(text.contains("- "), "should show deletions");
        assert!(text.contains("+ "), "should show additions");
    }

    #[test]
    fn write_file_shows_path_prominently() {
        let args = BTreeMap::from([
            ("path".to_owned(), "config.yaml".to_owned()),
            ("content".to_owned(), "key: value\nother: data".to_owned()),
        ]);
        let overlay = ApprovalOverlay::new("write_file".to_owned(), &args);
        let lines = overlay.build_content_lines(80);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("config.yaml"), "should show path");
    }
}

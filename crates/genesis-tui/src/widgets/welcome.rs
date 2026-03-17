//! Welcome screen widget.
//!
//! Displays a centered text-only startup screen with session metadata.

use ratatui::prelude::Widget;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::history::rgb;

const ACCENT: Color = rgb(genesis_ui::colors::UI_ACCENT);
const DIM: Color = rgb(genesis_ui::colors::UI_DIM);
const TEXT: Color = rgb(genesis_ui::colors::UI_TEXT);
const MUTED: Color = rgb(genesis_ui::colors::UI_MUTED);
const AMBER: Color = rgb(genesis_ui::colors::EVE_AMBER);
const SUCCESS: Color = rgb(genesis_ui::colors::UI_SUCCESS);
const PANEL_WIDTH: u16 = 60;

/// Session info displayed on the welcome screen.
pub struct WelcomeInfo {
    pub model: String,
    pub backend: String,
    pub session_id: String,
    pub cwd: String,
    pub version: String,
    pub tool_count_builtin: usize,
    pub tool_count_mcp: usize,
    pub skill_count: usize,
}

/// Welcome screen widget showing text-only session info.
pub struct WelcomeWidget {
    info: WelcomeInfo,
}

impl WelcomeWidget {
    /// Create a new welcome widget from session info.
    pub fn new(info: WelcomeInfo) -> Self {
        Self { info }
    }

    /// Render the welcome screen into the given area.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let panel_width = PANEL_WIDTH.min(area.width).max(24);
        let lines = self.build_lines(panel_width);
        if lines.is_empty() {
            return;
        }

        let content_height = lines.len() as u16;
        let panel_height = content_height.min(area.height);
        let panel_x = area.x + area.width.saturating_sub(panel_width) / 2;
        let panel_y = area.y + area.height.saturating_sub(panel_height) / 2;

        for (row_index, line) in lines.iter().take(panel_height as usize).enumerate() {
            let mut x = panel_x;
            let y = panel_y + row_index as u16;

            for span in &line.spans {
                for ch in span.content.chars() {
                    if x >= panel_x + panel_width {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        let mut s = String::with_capacity(ch.len_utf8());
                        s.push(ch);
                        cell.set_symbol(&s);
                        cell.set_style(span.style);
                    }
                    let char_width =
                        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
                    x = x.saturating_add(char_width.max(1));
                }
            }
        }
    }

    fn build_lines(&self, width: u16) -> Vec<Line<'static>> {
        let label_width = 9;
        let path_width = usize::from(width.saturating_sub(label_width + 2)).clamp(16, 40);
        let rule = "─".repeat(width as usize);
        let subtitle = format!("v{}  •  interactive coding session", self.info.version);
        let tools = if self.info.tool_count_mcp > 0 {
            format!(
                "{} builtin, {} mcp",
                self.info.tool_count_builtin,
                self.info.tool_count_mcp
            )
        } else {
            format!("{} builtin", self.info.tool_count_builtin)
        };

        vec![
            Line::from(Span::styled(
                ">_ Eve",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(subtitle, Style::default().fg(MUTED))),
            Line::from(""),
            Line::from(Span::styled(rule, Style::default().fg(DIM))),
            Line::from(""),
            info_row(label_width, "session", self.info.session_id.clone(), TEXT),
            info_row(label_width, "model", self.info.model.clone(), TEXT),
            info_row(label_width, "backend", self.info.backend.clone(), TEXT),
            info_row(
                label_width,
                "cwd",
                truncate_path(&self.info.cwd, path_width),
                TEXT,
            ),
            info_row(label_width, "tools", tools, SUCCESS),
            info_row(
                label_width,
                "skills",
                self.info.skill_count.to_string(),
                MUTED,
            ),
            Line::from(""),
            Line::from(Span::styled(
                "session ready",
                Style::default()
                    .fg(AMBER)
                    .add_modifier(Modifier::ITALIC),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("Enter   ", Style::default().fg(ACCENT)),
                Span::styled("send message", Style::default().fg(MUTED)),
                Span::styled("   /   ", Style::default().fg(ACCENT)),
                Span::styled("commands", Style::default().fg(MUTED)),
            ]),
            Line::from(vec![
                Span::styled("Ctrl+T  ", Style::default().fg(ACCENT)),
                Span::styled("transcript", Style::default().fg(MUTED)),
                Span::styled("   Ctrl+C  ", Style::default().fg(ACCENT)),
                Span::styled("interrupt", Style::default().fg(MUTED)),
            ]),
            Line::from(vec![
                Span::styled("Ctrl+D  ", Style::default().fg(ACCENT)),
                Span::styled("exit", Style::default().fg(MUTED)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "press any key to start",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
        ]
    }
}

fn info_row(label_width: usize, label: &str, value: String, value_color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:>width$}", width = label_width),
            Style::default().fg(DIM),
        ),
        Span::raw("  "),
        Span::styled(value, Style::default().fg(value_color)),
    ])
}

/// Truncate a path string to at most `max_len` characters.
fn truncate_path(path: &str, max_len: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max_len {
        path.to_owned()
    } else {
        let start = chars.len().saturating_sub(max_len - 3);
        let suffix: String = chars[start..].iter().collect();
        format!("...{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_rows(buf: &Buffer, width: u16) -> Vec<String> {
        buf.content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<Vec<_>>().join(""))
            .collect()
    }

    fn rendered_text(buf: &Buffer, width: u16) -> String {
        buffer_rows(buf, width).join("\n")
    }

    fn sample_info() -> WelcomeInfo {
        WelcomeInfo {
            model: "gpt-5.4".to_string(),
            backend: "openai-codex".to_string(),
            session_id: "cli-1773717043".to_string(),
            cwd: "/Users/coler/dev-personal/genesis".to_string(),
            version: "0.1.0".to_string(),
            tool_count_builtin: 73,
            tool_count_mcp: 0,
            skill_count: 1,
        }
    }

    #[test]
    fn welcome_widget_renders_dashboard_layout() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 100, 24));
        widget.render(Rect::new(0, 0, 100, 24), &mut buf);
        let rendered = rendered_text(&buf, 100);
        assert!(rendered.contains(">_ Eve"));
        assert!(rendered.contains("interactive coding session"));
        assert!(rendered.contains("session"));
        assert!(rendered.contains("press any key to start"));
        assert!(!rendered.contains("▀"));
        assert!(!rendered.contains("▄"));
    }

    #[test]
    fn welcome_widget_renders_narrow_text_only_layout() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 16));
        widget.render(Rect::new(0, 0, 40, 16), &mut buf);
        let rendered = rendered_text(&buf, 40);
        assert!(rendered.contains(">_ Eve"));
        assert!(rendered.contains("model"));
        assert!(!rendered.contains("▀"));
        assert!(!rendered.contains("▄"));
    }

    #[test]
    fn welcome_widget_zero_area_does_not_panic() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        widget.render(Rect::new(0, 0, 0, 0), &mut buf);
    }
}

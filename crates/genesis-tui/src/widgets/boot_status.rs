//! Boot status widget — system readiness lines for the boot sequence.
//!
//! Renders a compact set of subsystem status lines:
//!
//! ```text
//!    provider ··· ok
//!    storage  ··· ok
//!    tools    ··· ok
//!    mcp      ··· ok
//!    skills   ··· ok
//! ```
//!
//! Lines for MCP and skills are hidden when their respective counts are zero.
//! Unavailable subsystems show "--" in warning colour instead of "ok".

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::history::rgb;

/// Subsystem health summary for the boot status display.
pub struct BootStatusInfo {
    pub provider_ok: bool,
    pub storage_ok: bool,
    pub tool_count: usize,
    pub mcp_count: usize,
    pub skill_count: usize,
}

/// Renders system status lines for the boot sequence.
pub struct BootStatusWidget {
    info: BootStatusInfo,
}

const LABEL_COLOR: Color = rgb(genesis_ui::colors::EVE_LAVENDER);
const OK_COLOR: Color = rgb(genesis_ui::colors::UI_SUCCESS);
const WARN_COLOR: Color = rgb(genesis_ui::colors::UI_WARNING);
const SEP_COLOR: Color = rgb(genesis_ui::colors::UI_DIM);

impl BootStatusWidget {
    /// Create a new boot status widget from subsystem health info.
    pub fn new(info: BootStatusInfo) -> Self {
        Self { info }
    }

    /// Build the status lines to be rendered.
    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut result = Vec::with_capacity(5);

        result.push(status_line("provider", self.info.provider_ok));
        result.push(status_line("storage ", self.info.storage_ok));
        result.push(status_line("tools   ", self.info.tool_count > 0));

        if self.info.mcp_count > 0 {
            result.push(status_line("mcp     ", true));
        }

        if self.info.skill_count > 0 {
            result.push(status_line("skills  ", true));
        }

        result
    }

    /// Render the boot status into the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = self.lines();
        let paragraph = Paragraph::new(lines);
        paragraph.render(area, buf);
    }
}

/// Build a single status line: `   label ··· ok` or `   label ··· --`.
fn status_line(label: &str, ok: bool) -> Line<'static> {
    let (status_text, status_color) = if ok {
        ("ok", OK_COLOR)
    } else {
        ("--", WARN_COLOR)
    };

    Line::from(vec![
        Span::styled("   ", Style::default()),
        Span::styled(label.to_string(), Style::default().fg(LABEL_COLOR)),
        Span::styled(" ··· ", Style::default().fg(SEP_COLOR)),
        Span::styled(status_text, Style::default().fg(status_color)),
    ])
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn default_info() -> BootStatusInfo {
        BootStatusInfo {
            provider_ok: true,
            storage_ok: true,
            tool_count: 61,
            mcp_count: 3,
            skill_count: 5,
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn renders_all_five_lines_when_mcp_and_skills_present() {
        let widget = BootStatusWidget::new(default_info());
        let lines = widget.lines();
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn hides_mcp_line_when_mcp_count_zero() {
        let widget = BootStatusWidget::new(BootStatusInfo {
            mcp_count: 0,
            ..default_info()
        });
        let lines = widget.lines();
        assert_eq!(lines.len(), 4);
        // Verify no line contains "mcp"
        for line in &lines {
            assert!(
                !line_text(line).contains("mcp"),
                "mcp line should be hidden when mcp_count is 0"
            );
        }
    }

    #[test]
    fn hides_skills_line_when_skill_count_zero() {
        let widget = BootStatusWidget::new(BootStatusInfo {
            skill_count: 0,
            ..default_info()
        });
        let lines = widget.lines();
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert!(
                !line_text(line).contains("skills"),
                "skills line should be hidden when skill_count is 0"
            );
        }
    }

    #[test]
    fn hides_both_mcp_and_skills_when_zero() {
        let widget = BootStatusWidget::new(BootStatusInfo {
            mcp_count: 0,
            skill_count: 0,
            ..default_info()
        });
        let lines = widget.lines();
        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn shows_warning_for_unavailable_subsystem() {
        let widget = BootStatusWidget::new(BootStatusInfo {
            provider_ok: false,
            storage_ok: true,
            tool_count: 0,
            mcp_count: 0,
            skill_count: 0,
        });
        let lines = widget.lines();
        assert_eq!(lines.len(), 3);

        // Provider line should show "--"
        let provider_line = line_text(&lines[0]);
        assert!(
            provider_line.contains("--"),
            "provider should show '--' when unavailable, got: {provider_line}"
        );

        // Tools line should also show "--"
        let tools_line = line_text(&lines[2]);
        assert!(
            tools_line.contains("--"),
            "tools should show '--' when tool_count is 0, got: {tools_line}"
        );
    }

    #[test]
    fn shows_ok_for_available_subsystem() {
        let widget = BootStatusWidget::new(default_info());
        let lines = widget.lines();
        let provider_line = line_text(&lines[0]);
        assert!(
            provider_line.contains("ok"),
            "provider should show 'ok' when available, got: {provider_line}"
        );
    }

    #[test]
    fn render_does_not_panic_on_zero_area() {
        let widget = BootStatusWidget::new(default_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        widget.render(Rect::new(0, 0, 0, 0), &mut buf);
    }

    #[test]
    fn render_produces_content_in_buffer() {
        let widget = BootStatusWidget::new(default_info());
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        // Check that something was written
        let content: String = buf
            .content
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(
            content.contains("provider"),
            "buffer should contain 'provider'"
        );
    }
}

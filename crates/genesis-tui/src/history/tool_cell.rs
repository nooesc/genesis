//! ToolCell — renders a single tool invocation as a one-line summary.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;

/// Green colour for a successful tool call.
const COLOR_OK: Color = Color::Rgb(135, 175, 95);
/// Red colour for a failed tool call.
const COLOR_FAIL: Color = Color::Rgb(215, 95, 95);
/// Dim grey for structural characters.
const UI_DIM: Color = Color::Rgb(108, 108, 108);

/// A single tool invocation cell.
///
/// Renders as one line:
/// ```text
///   [tool_name] 1.2s ok
///   [tool_name] 0.5s FAIL
/// ```
#[derive(Debug, Clone)]
pub struct ToolCell {
    /// The tool's name (e.g. `"shell"`, `"read_file"`).
    pub tool_name: String,
    /// The provider-assigned call ID.
    pub call_id: String,
    /// A brief summary of the arguments (may be empty).
    pub args_summary: String,
    /// Whether the tool call succeeded.
    pub success: bool,
    /// How long the tool ran.
    pub duration: Duration,
}

impl ToolCell {
    /// Construct a new `ToolCell`.
    pub fn new(
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        args_summary: impl Into<String>,
        success: bool,
        duration: Duration,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            call_id: call_id.into(),
            args_summary: args_summary.into(),
            success,
            duration,
        }
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let lines = self.to_scrollback_lines(area.width);
        let paragraph = ratatui::widgets::Paragraph::new(lines);
        paragraph.render(area, buf);
    }

    /// Always returns 1 — tool cells are a single line.
    pub fn height(&self, _width: u16) -> u16 {
        1
    }

    /// Produce the single styled [`Line`] for scrollback insertion.
    pub fn to_scrollback_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let secs = self.duration.as_secs_f64();
        let duration_str = format!("{secs:.1}s");

        let (status_str, status_color) = if self.success {
            ("ok".to_string(), COLOR_OK)
        } else {
            ("FAIL".to_string(), COLOR_FAIL)
        };

        let line = Line::from(vec![
            // Two-space indent.
            Span::styled("  ", Style::default().fg(UI_DIM)),
            // [tool_name]
            Span::styled("[", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(UI_DIM)),
            Span::styled("] ", Style::default().fg(UI_DIM)),
            // duration
            Span::styled(duration_str, Style::default().fg(UI_DIM)),
            Span::styled(" ", Style::default()),
            // ok / FAIL
            Span::styled(status_str, Style::default().fg(status_color)),
        ]);

        vec![line]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cell(success: bool) -> ToolCell {
        ToolCell::new("shell", "call_1", "ls -la", success, Duration::from_millis(1200))
    }

    #[test]
    fn tool_cell_height_is_always_one() {
        assert_eq!(make_cell(true).height(80), 1);
        assert_eq!(make_cell(false).height(80), 1);
        assert_eq!(make_cell(true).height(20), 1);
    }

    #[test]
    fn tool_cell_scrollback_produces_one_line() {
        let lines = make_cell(true).to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn tool_cell_success_uses_green() {
        let cell = make_cell(true);
        let lines = cell.to_scrollback_lines(80);
        // Find the status span (last non-space span).
        let status_span = lines[0].spans.last().unwrap();
        assert_eq!(status_span.content, "ok");
        assert_eq!(status_span.style.fg, Some(COLOR_OK));
    }

    #[test]
    fn tool_cell_failure_uses_red() {
        let cell = make_cell(false);
        let lines = cell.to_scrollback_lines(80);
        let status_span = lines[0].spans.last().unwrap();
        assert_eq!(status_span.content, "FAIL");
        assert_eq!(status_span.style.fg, Some(COLOR_FAIL));
    }

    #[test]
    fn tool_cell_contains_tool_name() {
        let cell = make_cell(true);
        let lines = cell.to_scrollback_lines(80);
        let full_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            full_text.contains("shell"),
            "expected tool name in output: {full_text:?}"
        );
    }

    #[test]
    fn tool_cell_duration_format() {
        let cell = make_cell(true);
        let lines = cell.to_scrollback_lines(80);
        let full_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        // 1200ms → "1.2s"
        assert!(
            full_text.contains("1.2s"),
            "expected formatted duration in output: {full_text:?}"
        );
    }

    #[test]
    fn tool_cell_success_failure_differ() {
        let ok_lines = make_cell(true).to_scrollback_lines(80);
        let fail_lines = make_cell(false).to_scrollback_lines(80);
        // Status text differs.
        let ok_text: String = ok_lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let fail_text: String =
            fail_lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_ne!(ok_text, fail_text);
    }
}

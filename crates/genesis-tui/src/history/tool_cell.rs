//! ToolCell — renders a single tool invocation with configurable display modes.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;
use unicode_width::UnicodeWidthStr as _;

use super::rgb;

/// Green colour for a successful tool call.
const COLOR_OK: Color = rgb(genesis_ui::colors::UI_SUCCESS);
/// Red colour for a failed tool call.
const COLOR_FAIL: Color = rgb(genesis_ui::colors::UI_ERROR);
/// Dim grey for structural characters.
const UI_DIM: Color = rgb(genesis_ui::colors::UI_DIM);

/// Controls how a [`ToolCell`] is rendered.
#[derive(Debug, Clone, Copy, Default)]
pub enum ToolDisplayMode {
    /// Hide tool calls entirely.
    Off,
    /// Single line: `  [tool_name] 1.2s ok`
    Summary,
    /// Bordered block with tool name, args, duration, and status.
    #[default]
    Grouped,
    /// Like Grouped but also shows truncated output (if available).
    Verbose,
}

/// A single tool invocation cell.
///
/// Renders according to the configured [`ToolDisplayMode`].
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
    /// How to display this cell.
    pub display_mode: ToolDisplayMode,
    /// Optional output to show in Verbose mode.
    pub output: Option<String>,
}

impl ToolCell {
    /// Construct a new `ToolCell` with the given display mode.
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
            display_mode: ToolDisplayMode::default(),
            output: None,
        }
    }

    /// Set the display mode, returning `self` for builder-style chaining.
    pub fn with_display_mode(mut self, mode: ToolDisplayMode) -> Self {
        self.display_mode = mode;
        self
    }

    /// Set the optional output string (shown in Verbose mode).
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if matches!(self.display_mode, ToolDisplayMode::Off) {
            return;
        }
        let lines = self.to_scrollback_lines(area.width);
        let paragraph = ratatui::widgets::Paragraph::new(lines);
        paragraph.render(area, buf);
    }

    /// Return the number of rows this cell occupies at the given terminal width.
    pub fn height(&self, width: u16) -> u16 {
        let width = width.max(1);
        wrapped_row_count(&self.to_scrollback_lines(width), width)
    }

    /// Produce the styled [`Line`]s for scrollback insertion.
    pub fn to_scrollback_lines(&self, _width: u16) -> Vec<Line<'static>> {
        match self.display_mode {
            ToolDisplayMode::Off => vec![],
            ToolDisplayMode::Summary => self.summary_lines(),
            ToolDisplayMode::Grouped => self.grouped_lines(),
            ToolDisplayMode::Verbose => self.verbose_lines(),
        }
    }

    // ── Private helpers ────────────────────────────────────────────────────

    fn fmt_duration(&self) -> String {
        format!("{:.1}s", self.duration.as_secs_f64())
    }

    fn status_str(&self) -> (&'static str, Color) {
        if self.success {
            ("ok", COLOR_OK)
        } else {
            ("FAIL", COLOR_FAIL)
        }
    }

    /// Single-line summary: `  [tool_name] 1.2s ok`
    fn summary_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();

        let line = Line::from(vec![
            Span::styled("  ", Style::default().fg(UI_DIM)),
            Span::styled("[", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(UI_DIM)),
            Span::styled("] ", Style::default().fg(UI_DIM)),
            Span::styled(duration_str, Style::default().fg(UI_DIM)),
            Span::styled(" ", Style::default()),
            Span::styled(status_str, Style::default().fg(status_color)),
        ]);

        vec![line]
    }

    /// 4-line bordered block:
    /// ```text
    ///   ┌─ shell ──────────────────────────┐
    ///   │ args: echo "hello"               │
    ///   │ 1.2s ok                          │
    ///   └──────────────────────────────────┘
    /// ```
    fn grouped_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();

        // Top border: `  ┌─ tool_name ──...──┐`
        let top = Line::from(vec![
            Span::styled("  ┌─ ", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(UI_DIM)),
            Span::styled(" ─┐", Style::default().fg(UI_DIM)),
        ]);

        // Args line: `  │ args: <summary>   │`
        let args_line = Line::from(vec![
            Span::styled("  │ args: ", Style::default().fg(UI_DIM)),
            Span::styled(self.args_summary.clone(), Style::default().fg(UI_DIM)),
            Span::styled(" │", Style::default().fg(UI_DIM)),
        ]);

        // Status line: `  │ 1.2s ok          │`
        let status_line = Line::from(vec![
            Span::styled("  │ ", Style::default().fg(UI_DIM)),
            Span::styled(duration_str, Style::default().fg(UI_DIM)),
            Span::styled(" ", Style::default()),
            Span::styled(status_str, Style::default().fg(status_color)),
            Span::styled(" │", Style::default().fg(UI_DIM)),
        ]);

        // Bottom border: `  └─ … ─┘` matching the top border width.
        let fill_width = self.tool_name.len() + 2; // "─ " + name + " ─"
        let bottom_fill = "─".repeat(fill_width);
        let bottom = Line::from(vec![
            Span::styled(format!("  └{}┘", bottom_fill), Style::default().fg(UI_DIM)),
        ]);

        vec![top, args_line, status_line, bottom]
    }

    /// 4-line (or 5-line with output) bordered block including optional output.
    fn verbose_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();

        let top = Line::from(vec![
            Span::styled("  ┌─ ", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(UI_DIM)),
            Span::styled(" ─┐", Style::default().fg(UI_DIM)),
        ]);

        let args_line = Line::from(vec![
            Span::styled("  │ args: ", Style::default().fg(UI_DIM)),
            Span::styled(self.args_summary.clone(), Style::default().fg(UI_DIM)),
            Span::styled(" │", Style::default().fg(UI_DIM)),
        ]);

        let mut lines = vec![top, args_line];

        // Output line (only in Verbose mode, if output is present).
        if let Some(output) = &self.output {
            // Truncate output to a reasonable display length (character count, not bytes).
            let truncated = if output.chars().count() > 60 {
                let s: String = output.chars().take(60).collect();
                format!("{s}…")
            } else {
                output.clone()
            };
            let output_line = Line::from(vec![
                Span::styled("  │ output: ", Style::default().fg(UI_DIM)),
                Span::styled(truncated, Style::default().fg(UI_DIM)),
                Span::styled(" │", Style::default().fg(UI_DIM)),
            ]);
            lines.push(output_line);
        }

        let status_line = Line::from(vec![
            Span::styled("  │ ", Style::default().fg(UI_DIM)),
            Span::styled(duration_str, Style::default().fg(UI_DIM)),
            Span::styled(" ", Style::default()),
            Span::styled(status_str, Style::default().fg(status_color)),
            Span::styled(" │", Style::default().fg(UI_DIM)),
        ]);

        let fill_width = self.tool_name.len() + 2; // match top border
        let bottom_fill = "─".repeat(fill_width);
        let bottom = Line::from(vec![
            Span::styled(format!("  └{}┘", bottom_fill), Style::default().fg(UI_DIM)),
        ]);

        lines.push(status_line);
        lines.push(bottom);
        lines
    }
}

fn wrapped_row_count(lines: &[Line<'static>], wrap_width: u16) -> u16 {
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

    fn make_cell(success: bool) -> ToolCell {
        ToolCell::new("shell", "call_1", "ls -la", success, Duration::from_millis(1200))
    }

    // ── Legacy tests (kept for non-regression) ────────────────────────────

    #[test]
    fn tool_cell_height_is_always_one() {
        // Default mode is Grouped (height 4), so test Summary explicitly.
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Summary);
        assert_eq!(cell.height(80), 1);
        assert_eq!(make_cell(false).with_display_mode(ToolDisplayMode::Summary).height(80), 1);
        assert_eq!(make_cell(true).with_display_mode(ToolDisplayMode::Summary).height(20), 1);
    }

    #[test]
    fn tool_cell_scrollback_produces_one_line() {
        let lines = make_cell(true)
            .with_display_mode(ToolDisplayMode::Summary)
            .to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn tool_cell_success_uses_green() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Summary);
        let lines = cell.to_scrollback_lines(80);
        let status_span = lines[0].spans.last().unwrap();
        assert_eq!(status_span.content, "ok");
        assert_eq!(status_span.style.fg, Some(COLOR_OK));
    }

    #[test]
    fn tool_cell_failure_uses_red() {
        let cell = make_cell(false).with_display_mode(ToolDisplayMode::Summary);
        let lines = cell.to_scrollback_lines(80);
        let status_span = lines[0].spans.last().unwrap();
        assert_eq!(status_span.content, "FAIL");
        assert_eq!(status_span.style.fg, Some(COLOR_FAIL));
    }

    #[test]
    fn tool_cell_contains_tool_name() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Summary);
        let lines = cell.to_scrollback_lines(80);
        let full_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            full_text.contains("shell"),
            "expected tool name in output: {full_text:?}"
        );
    }

    #[test]
    fn tool_cell_duration_format() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Summary);
        let lines = cell.to_scrollback_lines(80);
        let full_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            full_text.contains("1.2s"),
            "expected formatted duration in output: {full_text:?}"
        );
    }

    #[test]
    fn tool_cell_success_failure_differ() {
        let ok_lines = make_cell(true)
            .with_display_mode(ToolDisplayMode::Summary)
            .to_scrollback_lines(80);
        let fail_lines = make_cell(false)
            .with_display_mode(ToolDisplayMode::Summary)
            .to_scrollback_lines(80);
        let ok_text: String = ok_lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let fail_text: String =
            fail_lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_ne!(ok_text, fail_text);
    }

    // ── New display mode tests ─────────────────────────────────────────────

    #[test]
    fn off_mode_zero_height() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Off);
        assert_eq!(cell.height(80), 0);
        assert_eq!(cell.height(20), 0);
    }

    #[test]
    fn off_mode_empty_scrollback() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Off);
        let lines = cell.to_scrollback_lines(80);
        assert!(lines.is_empty(), "Off mode should produce no lines");
    }

    #[test]
    fn summary_mode_single_line() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Summary);
        assert_eq!(cell.height(80), 1);
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("shell"), "summary should contain tool name");
        assert!(text.contains("1.2s"), "summary should contain duration");
    }

    #[test]
    fn grouped_mode_bordered_block() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Grouped);
        assert_eq!(cell.height(80), 4, "grouped mode should be 4 lines tall");
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 4);

        // Top border contains the tool name.
        let top: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(top.contains("shell"), "top border should contain tool name: {top:?}");
        assert!(top.contains("┌"), "top border should have box-drawing open: {top:?}");

        // Args line contains args_summary.
        let args: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(args.contains("ls -la"), "args line should contain args summary: {args:?}");

        // Status line contains duration and status.
        let status: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(status.contains("1.2s"), "status line should contain duration: {status:?}");
        assert!(status.contains("ok"), "status line should contain status: {status:?}");

        // Bottom border.
        let bottom: String = lines[3].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(bottom.contains("└"), "bottom border should have box-drawing close: {bottom:?}");
    }

    #[test]
    fn verbose_mode_shows_output() {
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Verbose)
            .with_output("hello");

        // With output: 5 lines (top + args + output + status + bottom).
        assert_eq!(cell.height(80), 5);
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 5);

        // Output line should be line index 2.
        let output_line: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            output_line.contains("output:"),
            "output line should have 'output:' label: {output_line:?}"
        );
        assert!(
            output_line.contains("hello"),
            "output line should contain the output text: {output_line:?}"
        );
    }

    #[test]
    fn verbose_mode_without_output_is_four_lines() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Verbose);
        assert_eq!(cell.height(80), 4);
        let lines = cell.to_scrollback_lines(80);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn grouped_mode_uses_dim_color_for_borders() {
        let cell = make_cell(true).with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        // The first span of the top border should use UI_DIM.
        let first_span = &lines[0].spans[0];
        assert_eq!(
            first_span.style.fg,
            Some(UI_DIM),
            "border spans should use UI_DIM color"
        );
    }

    #[test]
    fn verbose_output_truncated_at_60_chars() {
        let long_output = "a".repeat(80);
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Verbose)
            .with_output(long_output);
        let lines = cell.to_scrollback_lines(80);
        // output line is index 2
        let output_line: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        // Should contain ellipsis indicating truncation.
        assert!(
            output_line.contains('…'),
            "long output should be truncated with ellipsis: {output_line:?}"
        );
    }
}

//! ToolCell — renders a single tool invocation with configurable display modes.

use std::cell::Cell;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget as _;
use crate::render::diff;

use super::rgb;

/// Green colour for a successful tool call.
const COLOR_OK: Color = rgb(genesis_ui::colors::UI_SUCCESS);
/// Red colour for a failed tool call.
const COLOR_FAIL: Color = rgb(genesis_ui::colors::UI_ERROR);
/// Dim grey for structural characters.
const UI_DIM: Color = rgb(genesis_ui::colors::UI_DIM);

// ── Per-tool category colors ─────────────────────────────────────────────

/// Neutral dim for read-only tools.
const CAT_READ: Color = Color::Rgb(140, 140, 140);
/// Amber for write/modify tools.
const CAT_WRITE: Color = Color::Rgb(212, 165, 116);
/// Red for shell/execution tools.
const CAT_SHELL: Color = Color::Rgb(215, 95, 95);
/// Blue for web/network tools.
const CAT_WEB: Color = Color::Rgb(100, 149, 237);
/// Lavender for memory/skill/agent tools.
const CAT_AGENT: Color = rgb(genesis_ui::colors::EVE_LAVENDER);
/// Default tool color (text).
const CAT_DEFAULT: Color = rgb(genesis_ui::colors::UI_TEXT);

/// Return a category-based color for a tool name.
fn tool_color(name: &str) -> Color {
    match name {
        // Read-only tools
        "read_file" | "glob" | "tree" | "list_dir" | "search" | "grep"
        | "find_tools" | "session_info" | "echo" => CAT_READ,

        // Write/modify tools
        "write_file" | "patch_file" | "create_file" | "delete_file"
        | "rename_file" | "move_file" => CAT_WRITE,

        // Shell/execution tools
        "shell_exec" | "docker_exec" | "ssh_exec" | "process"
        | "kill_process" | "code_execute" => CAT_SHELL,

        // Web/network tools
        "web_search" | "web_fetch" | "browse" | "browser_navigate"
        | "browser_click" | "browser_fill" => CAT_WEB,

        // Agent/memory/skill tools
        "memory_search" | "memory_store" | "skill_create" | "skill_search"
        | "subagent_spawn" | "clarify" | "think" | "moa_consult"
        | "reason_with_model" => CAT_AGENT,

        // Tools with common prefixes
        _ if name.starts_with("git_") => CAT_WRITE,
        _ if name.starts_with("browser_") => CAT_WEB,
        _ if name.starts_with("mcp_") => CAT_AGENT,
        _ if name.starts_with("ha_") => CAT_WEB, // Home Assistant
        _ if name.starts_with("skill_") => CAT_AGENT,
        _ if name.starts_with("memory_") => CAT_AGENT,

        _ => CAT_DEFAULT,
    }
}

// ── Per-tool category dispatch ──────────────────────────────────────────

/// Categorize a tool for specialized rendering.
enum ToolCategory {
    Shell,     // shell_exec, docker_exec, ssh_exec, code_execute
    FileRead,  // read_file, list_dir
    FileWrite, // write_file, patch_file, create_file, delete_file, …
    Search,    // glob, search, grep, tree, find_tools
    Web,       // web_search, web_fetch, browse
    Default,   // everything else
}

fn tool_category(name: &str) -> ToolCategory {
    match name {
        "shell_exec" | "docker_exec" | "ssh_exec" | "code_execute" => ToolCategory::Shell,
        "read_file" | "list_dir" => ToolCategory::FileRead,
        "write_file" | "patch_file" | "create_file" | "delete_file" | "rename_file"
        | "move_file" => ToolCategory::FileWrite,
        "glob" | "search" | "grep" | "tree" | "find_tools" => ToolCategory::Search,
        "web_search" | "web_fetch" | "browse" => ToolCategory::Web,
        _ if name.starts_with("browser_") => ToolCategory::Web,
        _ if name.starts_with("git_") => ToolCategory::FileWrite,
        _ => ToolCategory::Default,
    }
}

/// Extract a file path from an args summary string.
///
/// The summary typically looks like `path: "src/main.rs"` or just `src/main.rs`.
fn extract_path(args_summary: &str) -> String {
    // Try to extract a quoted path first.
    if let Some(start) = args_summary.find('"') {
        if let Some(end) = args_summary[start + 1..].find('"') {
            return args_summary[start + 1..start + 1 + end].to_owned();
        }
    }
    // Otherwise use the part after "path:" or the whole summary.
    if let Some(rest) = args_summary.strip_prefix("path: ") {
        rest.trim().to_owned()
    } else {
        args_summary.to_owned()
    }
}

/// Extract a search pattern from an args summary string.
fn extract_pattern(args_summary: &str) -> String {
    if let Some(start) = args_summary.find('"') {
        if let Some(end) = args_summary[start + 1..].find('"') {
            return args_summary[start + 1..start + 1 + end].to_owned();
        }
    }
    if let Some(rest) = args_summary.strip_prefix("pattern: ") {
        rest.trim().to_owned()
    } else {
        args_summary.to_owned()
    }
}

/// Count results in tool output (lines as a rough result count).
fn count_results(output: &str) -> usize {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return 0;
    }
    trimmed.lines().count()
}

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
    /// Cached `(width, height)` to avoid recomputation when the width hasn't
    /// changed.  Uses interior mutability so `height()` can stay `&self`.
    cached_height: Cell<Option<(u16, u16)>>,
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
            cached_height: Cell::new(None),
        }
    }

    /// Set the display mode, returning `self` for builder-style chaining.
    pub fn with_display_mode(mut self, mode: ToolDisplayMode) -> Self {
        self.display_mode = mode;
        self.cached_height.set(None); // invalidate — display mode affects height
        self
    }

    /// Set the display mode on an existing cell, invalidating cached height.
    pub fn set_display_mode(&mut self, mode: ToolDisplayMode) {
        self.display_mode = mode;
        self.cached_height.set(None);
    }

    /// Set the optional output string (shown in Verbose mode).
    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self.cached_height.set(None); // invalidate — output affects Verbose height
        self
    }

    /// Render the cell into the given buffer area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if matches!(self.display_mode, ToolDisplayMode::Off) {
            return;
        }
        let lines = self.to_scrollback_lines(area.width);
        // Note: tool cells use bordered layouts (│ prefix) that are incompatible
        // with Paragraph::wrap(). Long lines are truncated, which matches the
        // height calculation below.
        let paragraph = ratatui::widgets::Paragraph::new(lines);
        paragraph.render(area, buf);
    }

    /// Return the number of rows this cell occupies at the given terminal width.
    pub fn height(&self, width: u16) -> u16 {
        if let Some((cached_w, cached_h)) = self.cached_height.get() {
            if cached_w == width {
                return cached_h;
            }
        }
        let w = width.max(1);
        let h = wrapped_row_count(&self.to_scrollback_lines(w), w);
        self.cached_height.set(Some((width, h)));
        h
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

    /// Produce per-tool-type content lines (between top border and status bar).
    fn tool_content_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        match tool_category(&self.tool_name) {
            ToolCategory::Shell => {
                // Show command prominently with a $ prompt.
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        "$ ",
                        Style::default()
                            .fg(CAT_SHELL)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(self.args_summary.clone(), Style::default().fg(CAT_SHELL)),
                ]));
            }
            ToolCategory::FileRead => {
                // Show file path with underline.
                let path = extract_path(&self.args_summary);
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        path,
                        Style::default()
                            .fg(CAT_READ)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ]));
            }
            ToolCategory::FileWrite => {
                // Show file path with write-colour underline.
                let path = extract_path(&self.args_summary);
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        path,
                        Style::default()
                            .fg(CAT_WRITE)
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ]));
                // If output contains a diff, render it inline.
                if let Some(output) = &self.output {
                    if diff::is_unified_diff(output) {
                        let diff_lines = diff::diff_to_lines(output);
                        for dl in diff_lines {
                            let mut spans =
                                vec![Span::styled("  │ ", Style::default().fg(UI_DIM))];
                            spans.extend(dl.spans);
                            lines.push(Line::from(spans));
                        }
                    }
                }
            }
            ToolCategory::Search => {
                // Show search pattern in /…/ notation and result count.
                let pattern = extract_pattern(&self.args_summary);
                let count = self
                    .output
                    .as_ref()
                    .map(|o| count_results(o))
                    .unwrap_or(0);
                let count_str = if count > 0 {
                    format!(" ({count} results)")
                } else {
                    String::new()
                };
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        format!("/{pattern}/"),
                        Style::default()
                            .fg(CAT_READ)
                            .add_modifier(Modifier::ITALIC),
                    ),
                    Span::styled(count_str, Style::default().fg(UI_DIM)),
                ]));
            }
            ToolCategory::Web => {
                // Show URL or query.
                lines.push(Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(self.args_summary.clone(), Style::default().fg(CAT_WEB)),
                ]));
            }
            ToolCategory::Default => {
                // Standard args line.
                lines.push(Line::from(vec![
                    Span::styled("  │ args: ", Style::default().fg(UI_DIM)),
                    Span::styled(self.args_summary.clone(), Style::default().fg(UI_DIM)),
                    Span::styled(" │", Style::default().fg(UI_DIM)),
                ]));
            }
        }

        lines
    }

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

    /// Extract the first line of the output, truncated to `max_chars` characters.
    ///
    /// Returns `None` if there is no output or the first line is blank.
    fn error_context(&self, max_chars: usize) -> Option<String> {
        let output = self.output.as_deref()?;
        let first_line = output.lines().find(|l| !l.trim().is_empty())?;
        let trimmed = first_line.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.chars().count() > max_chars {
            let s: String = trimmed.chars().take(max_chars).collect();
            Some(format!("{s}…"))
        } else {
            Some(trimmed.to_owned())
        }
    }

    /// Single-line summary: `  [tool_name] 1.2s ok`
    fn summary_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();
        let name_color = tool_color(&self.tool_name);

        let line = Line::from(vec![
            Span::styled("  ", Style::default().fg(UI_DIM)),
            Span::styled("[", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(name_color)),
            Span::styled("] ", Style::default().fg(UI_DIM)),
            Span::styled(duration_str, Style::default().fg(UI_DIM)),
            Span::styled(" ", Style::default()),
            Span::styled(status_str, Style::default().fg(status_color)),
        ]);

        vec![line]
    }

    /// Bordered block: tool name, per-category content, duration, status.
    fn grouped_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();

        // Top border: `  ┌─ tool_name ──...──┐`
        let name_color = tool_color(&self.tool_name);
        let top = Line::from(vec![
            Span::styled("  ┌─ ", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(name_color)),
            Span::styled(" ─┐", Style::default().fg(UI_DIM)),
        ]);

        let mut lines = vec![top];
        lines.extend(self.tool_content_lines());

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
        let bottom = Line::from(vec![Span::styled(
            format!("  └{}┘", bottom_fill),
            Style::default().fg(UI_DIM),
        )]);

        lines.push(status_line);

        // On failure, show a brief error reason (first line of output) if available.
        if !self.success {
            if let Some(reason) = self.error_context(60) {
                let error_line = Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        reason,
                        Style::default()
                            .fg(COLOR_FAIL)
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(" │", Style::default().fg(UI_DIM)),
                ]);
                lines.push(error_line);
            }
        }

        lines.push(bottom);
        lines
    }

    /// Bordered block including per-category content and optional verbose output.
    fn verbose_lines(&self) -> Vec<Line<'static>> {
        let duration_str = self.fmt_duration();
        let (status_str, status_color) = self.status_str();

        let name_color = tool_color(&self.tool_name);
        let top = Line::from(vec![
            Span::styled("  ┌─ ", Style::default().fg(UI_DIM)),
            Span::styled(self.tool_name.clone(), Style::default().fg(name_color)),
            Span::styled(" ─┐", Style::default().fg(UI_DIM)),
        ]);

        let mut lines = vec![top];
        lines.extend(self.tool_content_lines());

        // In verbose mode, also show plain-text output for categories that
        // don't already embed output in their content lines.
        if let Some(output) = &self.output {
            let category = tool_category(&self.tool_name);
            // FileWrite already renders diffs inline; Search shows result count.
            // Only skip verbose output when content_lines actually showed it.
            let already_shows_output = matches!(category, ToolCategory::Search)
                || (matches!(category, ToolCategory::FileWrite)
                    && diff::is_unified_diff(output));
            if !already_shows_output {
                if diff::is_unified_diff(output) {
                    // Render unified diff with colors, gutter, and line numbers.
                    let diff_lines = diff::diff_to_lines(output);
                    for dl in diff_lines {
                        let mut spans =
                            vec![Span::styled("  │ ", Style::default().fg(UI_DIM))];
                        spans.extend(dl.spans);
                        lines.push(Line::from(spans));
                    }
                } else {
                    // Plain text output — truncate to a reasonable display length.
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
            }
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
        let bottom = Line::from(vec![Span::styled(
            format!("  └{}┘", bottom_fill),
            Style::default().fg(UI_DIM),
        )]);

        lines.push(status_line);

        // On failure, show a brief error reason (first line of output) if available.
        // In verbose mode the output block above may already show the full text, but
        // the error context line on the status row still helps readability at a glance.
        if !self.success {
            if let Some(reason) = self.error_context(60) {
                let error_line = Line::from(vec![
                    Span::styled("  │ ", Style::default().fg(UI_DIM)),
                    Span::styled(
                        reason,
                        Style::default()
                            .fg(COLOR_FAIL)
                            .add_modifier(Modifier::DIM),
                    ),
                    Span::styled(" │", Style::default().fg(UI_DIM)),
                ]);
                lines.push(error_line);
            }
        }

        lines.push(bottom);
        lines
    }
}

/// Render a collapsed tool group as a single styled [`Line`].
///
/// Format: `  ▸ N tool calls (Xs, all ok)` or `  ▸ N tool calls (Xs, M failed)`
pub fn tool_group_summary_line(cells: &[&ToolCell]) -> Line<'static> {
    let count = cells.len();
    let total_dur: Duration = cells.iter().map(|c| c.duration).sum();
    let dur_str = format!("{:.1}s", total_dur.as_secs_f64());
    let fail_count = cells.iter().filter(|c| !c.success).count();

    let (status, status_color) = if fail_count == 0 {
        ("all ok".to_owned(), COLOR_OK)
    } else {
        (format!("{fail_count} failed"), COLOR_FAIL)
    };

    Line::from(vec![
        Span::styled("  ", Style::default().fg(UI_DIM)),
        Span::styled("▸ ", Style::default().fg(Color::Rgb(180, 167, 214))),
        Span::styled(
            format!("{count} tool calls"),
            Style::default().fg(Color::Rgb(168, 168, 168)),
        ),
        Span::styled(format!(" ({dur_str}, "), Style::default().fg(UI_DIM)),
        Span::styled(status, Style::default().fg(status_color)),
        Span::styled(")", Style::default().fg(UI_DIM)),
    ])
}

use super::wrapped_row_count;

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

    // ── Error context tests ───────────────────────────────────────────────

    #[test]
    fn grouped_failure_with_output_shows_error_context() {
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output("permission denied");
        let lines = cell.to_scrollback_lines(80);
        // grouped + error context: top + args + status + error + bottom = 5
        assert_eq!(lines.len(), 5, "grouped fail with output should be 5 lines, got {}", lines.len());
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("permission denied"),
            "grouped fail should show error context: {all_text:?}"
        );
    }

    #[test]
    fn grouped_failure_without_output_no_error_context_line() {
        let cell = make_cell(false).with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        // No output → no extra line: top + args + status + bottom = 4
        assert_eq!(lines.len(), 4, "grouped fail without output should be 4 lines");
    }

    #[test]
    fn grouped_success_with_output_no_error_context_line() {
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output("some output");
        let lines = cell.to_scrollback_lines(80);
        // success: error context is not shown even if there is output
        assert_eq!(lines.len(), 4, "grouped success should remain 4 lines");
    }

    #[test]
    fn verbose_failure_with_output_shows_error_context() {
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Verbose)
            .with_output("command not found");
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("command not found"),
            "verbose fail should show error context: {all_text:?}"
        );
    }

    #[test]
    fn summary_failure_with_output_no_error_context() {
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Summary)
            .with_output("permission denied");
        let lines = cell.to_scrollback_lines(80);
        // Summary is always exactly 1 line regardless of output.
        assert_eq!(lines.len(), 1, "summary mode should always be 1 line");
    }

    #[test]
    fn error_context_truncated_at_60_chars() {
        let long_error = "E".repeat(80);
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output(&long_error);
        let lines = cell.to_scrollback_lines(80);
        // Should have the error context line (index 3).
        let error_line: String = lines[3].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            error_line.contains('…'),
            "long error context should be truncated with ellipsis: {error_line:?}"
        );
    }

    #[test]
    fn error_context_uses_first_non_blank_line() {
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output("\n\nactual error on third line\nmore stuff");
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("actual error on third line"),
            "should show first non-blank line: {all_text:?}"
        );
        assert!(
            !all_text.contains("more stuff"),
            "should only show first line: {all_text:?}"
        );
    }

    #[test]
    fn error_context_span_uses_fail_color() {
        let cell = make_cell(false)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output("some error");
        let lines = cell.to_scrollback_lines(80);
        // error context line is at index 3 (top + args + status + error)
        let error_line = &lines[3];
        // The second span (index 1) holds the error text.
        let text_span = &error_line.spans[1];
        assert_eq!(
            text_span.style.fg,
            Some(COLOR_FAIL),
            "error context span should use COLOR_FAIL"
        );
    }

    // ── Diff rendering tests ──────────────────────────────────────────────

    const SAMPLE_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
 }";

    #[test]
    fn verbose_mode_renders_diff_with_colors() {
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Verbose)
            .with_output(SAMPLE_DIFF);
        let lines = cell.to_scrollback_lines(80);
        // Should have more than 5 lines (top + args + diff lines + status + bottom)
        assert!(
            lines.len() > 5,
            "diff output should produce more than 5 lines, got {}",
            lines.len()
        );
        // Check that some lines contain diff markers
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("+"), "diff should contain additions");
        assert!(all_text.contains("-"), "diff should contain deletions");
    }

    #[test]
    fn grouped_mode_shows_diff_for_file_write() {
        // FileWrite tools render inline diffs in grouped mode.
        let cell = ToolCell::new("patch_file", "call_1", r#"path: "src/main.rs""#, true, Duration::from_millis(300))
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output(SAMPLE_DIFF);
        let lines = cell.to_scrollback_lines(80);
        // Grouped with diff: top + path + diff lines + status + bottom > 4
        assert!(
            lines.len() > 4,
            "grouped FileWrite with diff should have more than 4 lines, got {}",
            lines.len()
        );
    }

    #[test]
    fn grouped_mode_no_diff_for_plain_output() {
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Grouped)
            .with_output("wrote 42 bytes to foo.txt");
        let lines = cell.to_scrollback_lines(80);
        // Plain output should not expand — grouped mode uses per-category content only
        assert_eq!(lines.len(), 4, "grouped with plain output should be 4 lines");
    }

    #[test]
    fn verbose_mode_plain_output_still_truncated() {
        let cell = make_cell(true)
            .with_display_mode(ToolDisplayMode::Verbose)
            .with_output("wrote 42 bytes to foo.txt");
        let lines = cell.to_scrollback_lines(80);
        // Plain output: top + args + output + status + bottom = 5
        assert_eq!(lines.len(), 5, "verbose with plain output should be 5 lines");
        let output_line: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(output_line.contains("output:"), "should have output: label");
    }

    // ── Per-tool category dispatch tests ─────────────────────────────────

    #[test]
    fn shell_tool_shows_dollar_prompt() {
        let cell =
            ToolCell::new("shell_exec", "call_1", "ls -la", true, Duration::from_millis(100))
                .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("$ "), "shell tool should show $ prompt");
        assert!(all_text.contains("ls -la"), "shell tool should show command");
    }

    #[test]
    fn read_file_shows_path() {
        let cell = ToolCell::new(
            "read_file",
            "call_1",
            r#"path: "src/main.rs""#,
            true,
            Duration::from_millis(50),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("src/main.rs"),
            "read_file should show path"
        );
    }

    #[test]
    fn search_tool_shows_pattern_and_count() {
        let cell = ToolCell::new(
            "grep",
            "call_1",
            r#"pattern: "TODO""#,
            true,
            Duration::from_millis(50),
        )
        .with_display_mode(ToolDisplayMode::Grouped)
        .with_output("file1.rs:10: TODO fix\nfile2.rs:20: TODO cleanup\nfile3.rs:5: TODO");
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(all_text.contains("TODO"), "search should show pattern");
        assert!(
            all_text.contains("3 results"),
            "search should show result count"
        );
    }

    #[test]
    fn web_tool_shows_url() {
        let cell = ToolCell::new(
            "web_fetch",
            "call_1",
            "https://example.com",
            true,
            Duration::from_millis(200),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("https://example.com"),
            "web tool should show URL"
        );
    }

    #[test]
    fn default_tool_shows_args() {
        let cell = ToolCell::new(
            "custom_tool",
            "call_1",
            "arg1: val1",
            true,
            Duration::from_millis(50),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("args:"),
            "default tool should show args label"
        );
    }

    #[test]
    fn extract_path_quoted() {
        assert_eq!(extract_path(r#"path: "src/main.rs""#), "src/main.rs");
    }

    #[test]
    fn extract_path_unquoted() {
        assert_eq!(extract_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn extract_path_strip_prefix() {
        assert_eq!(extract_path("path: src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn extract_pattern_quoted() {
        assert_eq!(extract_pattern(r#"pattern: "TODO""#), "TODO");
    }

    #[test]
    fn extract_pattern_unquoted() {
        assert_eq!(extract_pattern("TODO"), "TODO");
    }

    #[test]
    fn count_results_counts_lines() {
        assert_eq!(count_results("a\nb\nc"), 3);
        assert_eq!(count_results(""), 0);
        assert_eq!(count_results("single"), 1);
    }

    #[test]
    fn file_write_grouped_shows_path() {
        let cell = ToolCell::new(
            "write_file",
            "call_1",
            r#"path: "out.txt""#,
            true,
            Duration::from_millis(20),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("out.txt"),
            "write_file should show path: {all_text:?}"
        );
    }

    #[test]
    fn git_tool_is_file_write_category() {
        let cell = ToolCell::new(
            "git_commit",
            "call_1",
            "commit -m fix",
            true,
            Duration::from_millis(100),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        // git_ tools are FileWrite category — should show path-style underlined content
        // (not the Default "args:" label)
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !all_text.contains("args:"),
            "git_ tool should not use default args: label"
        );
    }

    #[test]
    fn browser_tool_is_web_category() {
        let cell = ToolCell::new(
            "browser_navigate",
            "call_1",
            "https://rust-lang.org",
            true,
            Duration::from_millis(500),
        )
        .with_display_mode(ToolDisplayMode::Grouped);
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            all_text.contains("https://rust-lang.org"),
            "browser_ tool should show URL"
        );
        assert!(
            !all_text.contains("args:"),
            "browser_ tool should not use default args: label"
        );
    }

    // ── Tool group summary tests ──────────────────────────────────────────

    #[test]
    fn tool_group_summary_all_ok() {
        let c1 = ToolCell::new("shell", "c1", "ls", true, Duration::from_millis(500));
        let c2 = ToolCell::new("read_file", "c2", "f.rs", true, Duration::from_millis(300));
        let c3 = ToolCell::new("grep", "c3", "pat", true, Duration::from_millis(200));

        let line = tool_group_summary_line(&[&c1, &c2, &c3]);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("3 tool calls"), "should show count: {text:?}");
        assert!(text.contains("1.0s"), "should show total duration: {text:?}");
        assert!(text.contains("all ok"), "should show all ok: {text:?}");
    }

    #[test]
    fn tool_group_summary_with_failures() {
        let c1 = ToolCell::new("shell", "c1", "ls", true, Duration::from_millis(400));
        let c2 = ToolCell::new("shell", "c2", "rm x", false, Duration::from_millis(100));
        let c3 = ToolCell::new("shell", "c3", "cat y", false, Duration::from_millis(200));

        let line = tool_group_summary_line(&[&c1, &c2, &c3]);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("3 tool calls"), "should show count: {text:?}");
        assert!(text.contains("0.7s"), "should show total duration: {text:?}");
        assert!(text.contains("2 failed"), "should show failure count: {text:?}");

        // Verify that the failure count span uses COLOR_FAIL.
        let fail_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("failed"))
            .unwrap();
        assert_eq!(
            fail_span.style.fg,
            Some(COLOR_FAIL),
            "failure text should use COLOR_FAIL"
        );
    }

    #[test]
    fn verbose_search_skips_duplicate_output() {
        // Search tools already embed result count in content lines, so verbose
        // mode should NOT also show a separate "output:" line.
        let cell = ToolCell::new(
            "grep",
            "call_1",
            r#"pattern: "fn""#,
            true,
            Duration::from_millis(30),
        )
        .with_display_mode(ToolDisplayMode::Verbose)
        .with_output("main.rs:1: fn main()\nlib.rs:5: fn helper()");
        let lines = cell.to_scrollback_lines(80);
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            !all_text.contains("output:"),
            "search in verbose mode should not duplicate output: {all_text:?}"
        );
        assert!(
            all_text.contains("2 results"),
            "search should show result count"
        );
    }
}

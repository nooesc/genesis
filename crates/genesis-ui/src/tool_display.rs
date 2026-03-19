//! Tool call display formatting for the CLI.
//!
//! Provides a [`ToolCallBuffer`] that accumulates tool call start/end events
//! and renders them as a grouped, summary, or verbose block of text.

use std::fmt::Write;
use std::time::Duration;

use owo_colors::OwoColorize;

use crate::UiContext;

/// Display mode for tool call progress output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolDisplayMode {
    /// Do not show tool call progress.
    Off,
    /// Show a brief one-line summary per tool call.
    Summary,
    /// Group tool calls in a bordered block (default).
    #[default]
    Grouped,
    /// Show full tool call details (grouped + truncated args).
    Verbose,
}

/// A completed tool call with timing and result metadata.
#[derive(Debug)]
struct CompletedToolCall {
    name: String,
    args_summary: String,
    duration: Duration,
    success: bool,
}

/// Accumulates tool call events and renders formatted output.
pub struct ToolCallBuffer {
    calls: Vec<CompletedToolCall>,
    mode: ToolDisplayMode,
    pending: Option<(String, String)>, // (name, args_summary) of currently executing tool
}

impl ToolCallBuffer {
    /// Create a new buffer with the given display mode.
    pub fn new(mode: ToolDisplayMode) -> Self {
        Self {
            calls: Vec::new(),
            mode,
            pending: None,
        }
    }

    /// Record that a tool call has started executing.
    pub fn on_tool_start(&mut self, name: &str, args_summary: &str) {
        self.pending = Some((name.to_owned(), args_summary.to_owned()));
    }

    /// Record that a tool call has finished executing.
    pub fn on_tool_end(&mut self, name: &str, duration: Duration, success: bool) {
        let args_summary = if let Some((ref pending_name, ref summary)) = self.pending {
            if pending_name == name {
                let s = summary.clone();
                self.pending = None;
                s
            } else {
                self.pending = None;
                String::new()
            }
        } else {
            String::new()
        };

        self.calls.push(CompletedToolCall {
            name: name.to_owned(),
            args_summary,
            duration,
            success,
        });
    }

    /// Returns `true` if no tool calls have been buffered.
    pub fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }

    /// Render the buffered tool calls and clear the buffer.
    ///
    /// Returns `None` if the buffer is empty or the mode is [`ToolDisplayMode::Off`].
    pub fn flush(&mut self, ui: &UiContext) -> Option<String> {
        if self.calls.is_empty() || self.mode == ToolDisplayMode::Off {
            self.calls.clear();
            return None;
        }

        let result = match self.mode {
            ToolDisplayMode::Summary => Some(self.render_summary(ui)),
            ToolDisplayMode::Grouped => Some(self.render_grouped(ui, false)),
            ToolDisplayMode::Verbose => Some(self.render_grouped(ui, true)),
            ToolDisplayMode::Off => unreachable!("Off mode is handled by early return above"),
        };

        self.calls.clear();
        result
    }

    /// Render each tool call as a single summary line.
    fn render_summary(&self, ui: &UiContext) -> String {
        let styles = ui.styles();
        let mut out = String::new();

        for call in &self.calls {
            let args_display = truncate_display(&call.args_summary, 30);
            let timing = format_duration(call.duration);
            let status = if call.success { "ok" } else { "failed" };

            if ui.colors_enabled {
                let _ = write!(
                    out,
                    "  {} {:14}{:>32}{:>6}  {}",
                    "\u{00b7}".style(styles.dim),
                    call.name.style(styles.muted),
                    args_display.style(styles.text),
                    timing.style(styles.dim),
                    if call.success {
                        status.style(styles.success).to_string()
                    } else {
                        status.style(styles.error).to_string()
                    },
                );
            } else {
                let _ = write!(
                    out,
                    "  \u{00b7} {:14}{:>32}{:>6}  {}",
                    call.name, args_display, timing, status,
                );
            }
            out.push('\n');
        }

        // Remove trailing newline
        if out.ends_with('\n') {
            out.pop();
        }
        out
    }

    /// Render tool calls in a bordered grouped block.
    fn render_grouped(&self, ui: &UiContext, verbose: bool) -> String {
        let styles = ui.styles();
        let mut out = String::new();

        // Header
        let header = "  \u{250c} tools \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}";
        if ui.colors_enabled {
            let _ = writeln!(out, "{}", header.style(styles.dim));
        } else {
            let _ = writeln!(out, "{header}");
        }

        // Tool call rows
        let mut total_duration = Duration::ZERO;
        for call in &self.calls {
            total_duration += call.duration;
            let args_display = truncate_display(&call.args_summary, 30);
            let timing = format_duration(call.duration);
            let status = if call.success { "ok" } else { "failed" };

            if ui.colors_enabled {
                let _ = write!(
                    out,
                    "  {} {:14}{:>30}{:>6}  {}",
                    "\u{2502}".style(styles.dim),
                    call.name.style(styles.muted),
                    args_display.style(styles.text),
                    timing.style(styles.dim),
                    if call.success {
                        status.style(styles.success).to_string()
                    } else {
                        status.style(styles.error).to_string()
                    },
                );
            } else {
                let _ = write!(
                    out,
                    "  \u{2502} {:14}{:>30}{:>6}  {}",
                    call.name, args_display, timing, status,
                );
            }
            out.push('\n');

            // Verbose mode: show full args below the row
            if verbose && !call.args_summary.is_empty() {
                let verbose_args = truncate_display(&call.args_summary, 50);
                if ui.colors_enabled {
                    let _ = writeln!(
                        out,
                        "  {}   -> \"{}\"",
                        "\u{2502}".style(styles.dim),
                        verbose_args.style(styles.dim),
                    );
                } else {
                    let _ = writeln!(out, "  \u{2502}   -> \"{verbose_args}\"");
                }
            }
        }

        // Footer
        let total_str = format_duration(total_duration);
        let count = self.calls.len();
        let footer = format!(
            "  \u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500} {count} call{}, {total_str} total \u{2500}",
            if count == 1 { "" } else { "s" },
        );
        if ui.colors_enabled {
            let _ = write!(out, "{}", footer.style(styles.dim));
        } else {
            let _ = write!(out, "{footer}");
        }

        out
    }
}

/// Truncate a string for display, adding "..." if it exceeds `max_len` characters.
fn truncate_display(s: &str, max_len: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_len {
        s.to_owned()
    } else {
        let truncated: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Format a Duration as a human-friendly short string like "0.4s" or "3.2s".
fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 10.0 {
        format!("{secs:.1}s")
    } else {
        format!("{:.0}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ColorMode;

    fn ui_plain() -> UiContext {
        UiContext::new(ColorMode::Never)
    }

    #[test]
    fn empty_buffer_returns_none() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Grouped);
        assert!(buf.is_empty());
        assert!(buf.flush(&ui_plain()).is_none());
    }

    #[test]
    fn off_mode_returns_none() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Off);
        buf.on_tool_start("shell_exec", "git status");
        buf.on_tool_end("shell_exec", Duration::from_millis(400), true);
        assert!(buf.flush(&ui_plain()).is_none());
    }

    #[test]
    fn flush_clears_buffer() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Summary);
        buf.on_tool_start("read_file", "src/main.rs");
        buf.on_tool_end("read_file", Duration::from_millis(100), true);
        assert!(!buf.is_empty());
        let _ = buf.flush(&ui_plain());
        assert!(buf.is_empty());
    }

    #[test]
    fn summary_mode_output() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Summary);
        buf.on_tool_start("shell_exec", "git status");
        buf.on_tool_end("shell_exec", Duration::from_millis(400), true);
        buf.on_tool_start("read_file", "src/main.rs");
        buf.on_tool_end("read_file", Duration::from_millis(100), true);

        let output = buf.flush(&ui_plain()).unwrap();
        assert!(output.contains("shell_exec"));
        assert!(output.contains("read_file"));
        assert!(output.contains("ok"));
        assert!(output.contains("0.4s"));
        assert!(output.contains("0.1s"));
    }

    #[test]
    fn grouped_mode_output() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Grouped);
        buf.on_tool_start("shell_exec", "git status");
        buf.on_tool_end("shell_exec", Duration::from_millis(400), true);
        buf.on_tool_start("read_file", "src/main.rs");
        buf.on_tool_end("read_file", Duration::from_millis(100), true);
        buf.on_tool_start("shell_exec", "cargo test");
        buf.on_tool_end(
            "shell_exec",
            Duration::from_secs(3) + Duration::from_millis(200),
            false,
        );

        let output = buf.flush(&ui_plain()).unwrap();
        assert!(output.contains("\u{250c} tools"));
        assert!(output.contains("\u{2514}"));
        assert!(output.contains("3 calls"));
        assert!(output.contains("total"));
        assert!(output.contains("shell_exec"));
        assert!(output.contains("ok"));
        assert!(output.contains("failed"));
    }

    #[test]
    fn verbose_mode_shows_args() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Verbose);
        buf.on_tool_start("shell_exec", "git status --short");
        buf.on_tool_end("shell_exec", Duration::from_millis(400), true);

        let output = buf.flush(&ui_plain()).unwrap();
        assert!(output.contains("-> \"git status --short\""));
    }

    #[test]
    fn single_call_footer() {
        let mut buf = ToolCallBuffer::new(ToolDisplayMode::Grouped);
        buf.on_tool_start("read_file", "foo.rs");
        buf.on_tool_end("read_file", Duration::from_millis(50), true);

        let output = buf.flush(&ui_plain()).unwrap();
        assert!(output.contains("1 call,"));
        assert!(!output.contains("1 calls"));
    }

    #[test]
    fn truncate_display_long_args() {
        let long = "a".repeat(50);
        let result = truncate_display(&long, 30);
        assert!(result.len() <= 30);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn truncate_display_short_args() {
        let short = "hello";
        assert_eq!(truncate_display(short, 30), "hello");
    }

    #[test]
    fn format_duration_subsecond() {
        assert_eq!(format_duration(Duration::from_millis(400)), "0.4s");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(
            format_duration(Duration::from_secs(3) + Duration::from_millis(200)),
            "3.2s"
        );
    }

    #[test]
    fn format_duration_large() {
        assert_eq!(format_duration(Duration::from_secs(15)), "15s");
    }
}

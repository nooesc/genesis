//! Status bar widget — a single-row footer with rich session info.
//!
//! Layout (always 1 row, full width):
//!
//! **Idle:**
//!   ` ◆ model · ctx% ─── session_id ─── ⎇ branch `
//!
//! **Active (thinking/streaming/tool):**
//!   ` ◆ model · ctx% ─── (~'.')~ thinking ─── ⎇ branch  ↑in ↓out `
//!
//! The bar has a subtle background tint during active operations and
//! the Eve dance sprite `(~'.')~` / `~('.'~)` animates in the center.

use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Span,
};
use unicode_width::UnicodeWidthChar;

use crate::events::StatusState;

// ── Palette ─────────────────────────────────────────────────────────────────

/// Background tint for the bar (very subtle dark).
const BAR_BG: Color = Color::Rgb(30, 28, 36);
/// Background tint when active (slightly brighter).
const BAR_BG_ACTIVE: Color = Color::Rgb(38, 34, 48);
/// Dim text for labels and secondary info.
const DIM: Color = Color::Rgb(98, 98, 98);
/// Standard text.
const TEXT: Color = Color::Rgb(168, 168, 168);
/// Accent (Eve lavender) for the diamond and active labels.
const ACCENT: Color = Color::Rgb(180, 167, 214);
/// Success green for the branch icon.
const BRANCH_COLOR: Color = Color::Rgb(135, 175, 95);
/// Muted for separators.
const SEP_COLOR: Color = Color::Rgb(58, 55, 66);
/// Token count color.
const TOKEN_COLOR: Color = Color::Rgb(138, 138, 138);
/// Active spinner/dance color.
const DANCE_COLOR: Color = Color::Rgb(180, 167, 214);
/// Tool name color (amber).
const TOOL_COLOR: Color = Color::Rgb(212, 165, 116);

// ── Animation data ──────────────────────────────────────────────────────────

/// Four-frame Eve dance sprites for a smooth sway.
const EVE_SPRITES: [&str; 4] = ["(~'.')~", "~('.'~)", "(~'.')~", " ('.')>"];

/// Ten-frame Braille spinner for tool execution.
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Interval between dance frame advances (thinking).
const DANCE_INTERVAL: Duration = Duration::from_millis(400);

/// Interval between spinner frame advances (tool/streaming).
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

// ─────────────────────────────────────────────────────────────────────────────

/// A single-row status bar rendered at the bottom of the TUI viewport.
pub struct StatusBarWidget {
    /// Currently active model name.
    pub model: String,
    /// Context window usage percentage (0–100).
    pub context_percent: u8,
    /// Current agent state.
    pub state: StatusState,
    /// Current animation frame index.
    pub sprite_frame: usize,
    /// When the last frame advance happened.
    pub last_tick: Instant,
    /// Git branch name (or cwd fallback). `None` until first populated.
    right_info: Option<String>,
    /// Cumulative input tokens this session.
    pub tokens_in: u32,
    /// Cumulative output tokens this session.
    pub tokens_out: u32,
    /// Elapsed time since the current turn started.
    pub turn_elapsed: Option<Duration>,
}

impl StatusBarWidget {
    /// Create a new status bar.
    ///
    /// Git branch detection is deferred to the first `render()` call so the
    /// constructor never blocks the async runtime with a synchronous subprocess.
    pub fn new(model: String) -> Self {
        Self {
            model,
            context_percent: 0,
            state: StatusState::Idle,
            sprite_frame: 0,
            last_tick: Instant::now(),
            right_info: None,
            tokens_in: 0,
            tokens_out: 0,
            turn_elapsed: None,
        }
    }

    /// Ensure `right_info` is populated, running the git subprocess if needed.
    ///
    /// This is intentionally synchronous but called at most once per session,
    /// from `render()`, which runs in the terminal draw callback.
    fn ensure_right_info(&mut self) {
        if self.right_info.is_none() {
            self.right_info = Some(Self::detect_right_info());
        }
    }

    /// Update the current agent state.
    pub fn set_state(&mut self, state: StatusState) {
        self.state = state;
        self.sprite_frame = 0;
        self.last_tick = Instant::now();
    }

    /// Update the displayed model name.
    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Update the context usage percentage.
    pub fn set_context_percent(&mut self, pct: u8) {
        self.context_percent = pct;
    }

    /// Whether the status bar is in an animated state (needs periodic redraws).
    pub fn is_animating(&self) -> bool {
        !matches!(self.state, StatusState::Idle)
    }

    /// The preferred animation interval for the current state.
    pub fn animation_interval(&self) -> Duration {
        match &self.state {
            StatusState::Thinking => DANCE_INTERVAL,
            StatusState::ToolRunning { .. } | StatusState::Streaming { .. } => SPINNER_INTERVAL,
            StatusState::Idle => Duration::from_secs(3600),
        }
    }

    /// Advance the animation frame if enough time has elapsed.
    pub fn tick(&mut self) {
        if matches!(self.state, StatusState::Idle) {
            return;
        }

        let interval = self.animation_interval();
        if self.last_tick.elapsed() >= interval {
            self.sprite_frame = self.sprite_frame.wrapping_add(1);
            self.last_tick = Instant::now();
        }
    }

    /// Render the status bar into `buf` within `area`.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        self.ensure_right_info();
        if area.height == 0 || area.width == 0 {
            return;
        }

        let row = area.y;
        let is_active = self.is_animating();
        let bg = if is_active { BAR_BG_ACTIVE } else { BAR_BG };

        // Fill entire row with background.
        let bg_style = Style::default().bg(bg);
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_char(' ');
                cell.set_style(bg_style);
            }
        }

        // Build sections.
        let left = self.build_left(bg);
        let center = self.build_center(bg);
        let right = self.build_right(bg);

        let total = area.width as usize;
        let left_w = spans_width(&left);
        let center_w = spans_width(&center);
        let right_w = spans_width(&right);

        // Position sections.
        let center_start = if total > center_w {
            (total / 2).saturating_sub(center_w / 2)
        } else {
            left_w + 1
        };
        let right_start = total.saturating_sub(right_w + 1);

        // Draw left (with 1-col left padding).
        write_spans(&left, area.x + 1, row, area.x + area.width, buf);

        // Draw fill between left and center.
        if center_w > 0 {
            let fill_start = area.x + 1 + left_w as u16 + 1;
            let fill_end = area.x + center_start as u16;
            if fill_start < fill_end {
                let fill_style = Style::default().fg(SEP_COLOR).bg(bg);
                for x in fill_start..fill_end {
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        cell.set_symbol("─");
                        cell.set_style(fill_style);
                    }
                }
            }

            // Draw center.
            write_spans(&center, area.x + center_start as u16, row, area.x + area.width, buf);

            // Draw fill between center and right.
            let center_end = area.x + center_start as u16 + center_w as u16 + 1;
            let right_x = area.x + right_start as u16;
            if center_end < right_x {
                let fill_style = Style::default().fg(SEP_COLOR).bg(bg);
                for x in center_end..right_x {
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        cell.set_symbol("─");
                        cell.set_style(fill_style);
                    }
                }
            }
        } else {
            // No center — fill between left and right.
            let fill_start = area.x + 1 + left_w as u16 + 1;
            let right_x = area.x + right_start as u16;
            if fill_start < right_x {
                let fill_style = Style::default().fg(SEP_COLOR).bg(bg);
                for x in fill_start..right_x {
                    if let Some(cell) = buf.cell_mut((x, row)) {
                        cell.set_symbol("─");
                        cell.set_style(fill_style);
                    }
                }
            }
        }

        // Draw right.
        write_spans(&right, area.x + right_start as u16, row, area.x + area.width, buf);
    }

    // ── Section builders ─────────────────────────────────────────────────

    fn build_left(&self, bg: Color) -> Vec<Span<'static>> {
        let mut spans = Vec::with_capacity(4);

        // Diamond accent.
        spans.push(Span::styled(
            "◆ ",
            Style::default().fg(ACCENT).bg(bg),
        ));

        // Model name.
        spans.push(Span::styled(
            self.model.clone(),
            Style::default().fg(TEXT).bg(bg).add_modifier(Modifier::BOLD),
        ));

        // Context %.
        let ctx = format!(" · {}%", self.context_percent);
        spans.push(Span::styled(
            ctx,
            Style::default().fg(DIM).bg(bg),
        ));

        spans
    }

    fn build_center(&self, bg: Color) -> Vec<Span<'static>> {
        match &self.state {
            StatusState::Idle => vec![],

            StatusState::Thinking => {
                let frame = self.sprite_frame % EVE_SPRITES.len();
                let sprite = EVE_SPRITES[frame];
                let elapsed = self.format_elapsed();
                vec![
                    Span::styled(
                        format!(" {sprite} "),
                        Style::default().fg(DANCE_COLOR).bg(bg),
                    ),
                    Span::styled(
                        format!("thinking {elapsed}"),
                        Style::default().fg(DIM).bg(bg),
                    ),
                ]
            }

            StatusState::ToolRunning { tool_name } => {
                let spinner = SPINNER_FRAMES[self.sprite_frame % SPINNER_FRAMES.len()];
                let elapsed = self.format_elapsed();
                vec![
                    Span::styled(
                        format!(" {spinner} "),
                        Style::default().fg(DANCE_COLOR).bg(bg),
                    ),
                    Span::styled(
                        tool_name.clone(),
                        Style::default().fg(TOOL_COLOR).bg(bg),
                    ),
                    Span::styled(
                        format!(" {elapsed}"),
                        Style::default().fg(DIM).bg(bg),
                    ),
                ]
            }

            StatusState::Streaming { tokens } => {
                let spinner = SPINNER_FRAMES[self.sprite_frame % SPINNER_FRAMES.len()];
                vec![
                    Span::styled(
                        format!(" {spinner} "),
                        Style::default().fg(DANCE_COLOR).bg(bg),
                    ),
                    Span::styled(
                        format!("streaming · {tokens} tok"),
                        Style::default().fg(DIM).bg(bg),
                    ),
                ]
            }
        }
    }

    fn build_right(&self, bg: Color) -> Vec<Span<'static>> {
        let mut spans = Vec::with_capacity(4);

        // Token counts (only when we have some).
        if self.tokens_in > 0 || self.tokens_out > 0 {
            spans.push(Span::styled(
                format!("↑{} ↓{}", format_tokens(self.tokens_in), format_tokens(self.tokens_out)),
                Style::default().fg(TOKEN_COLOR).bg(bg),
            ));
            spans.push(Span::styled(
                " · ",
                Style::default().fg(SEP_COLOR).bg(bg),
            ));
        }

        // Branch name with icon.
        spans.push(Span::styled(
            "⎇ ",
            Style::default().fg(BRANCH_COLOR).bg(bg),
        ));
        spans.push(Span::styled(
            self.right_info.clone().unwrap_or_default(),
            Style::default().fg(DIM).bg(bg),
        ));

        spans
    }

    fn format_elapsed(&self) -> String {
        match self.turn_elapsed {
            Some(d) if d.as_secs() >= 60 => format!("{}m{}s", d.as_secs() / 60, d.as_secs() % 60),
            Some(d) => format!("{:.1}s", d.as_secs_f64()),
            None => String::new(),
        }
    }

    fn detect_right_info() -> String {
        if let Ok(output) = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
        {
            if output.status.success() {
                let branch = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_string();
                if !branch.is_empty() {
                    return branch;
                }
            }
        }
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "~".to_string())
    }
}

// ── Convenience ─────────────────────────────────────────────────────────────

impl StatusBarWidget {
    /// Render to a ratatui `Line` for tests.
    pub fn to_line(&self) -> ratatui::text::Line<'static> {
        let bg = BAR_BG;
        let mut spans = Vec::new();
        spans.extend(self.build_left(bg));
        let center = self.build_center(bg);
        if !center.is_empty() {
            spans.push(Span::raw("  "));
            spans.extend(center);
        }
        let right = self.build_right(bg);
        if !right.is_empty() {
            spans.push(Span::raw("  "));
            spans.extend(right);
        }
        ratatui::text::Line::from(spans)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Compute the display width of a slice of spans.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .flat_map(|s| s.content.chars())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// Write spans into the buffer starting at (start_x, row), clipped at bound_x.
fn write_spans(spans: &[Span<'_>], start_x: u16, row: u16, bound_x: u16, buf: &mut Buffer) {
    let mut x = start_x;
    for span in spans {
        for ch in span.content.chars() {
            if x >= bound_x {
                return;
            }
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_char(ch);
                cell.set_style(span.style);
            }
            x += UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
        }
    }
}

/// Format token counts compactly: 1234 → "1.2k", 12345 → "12k".
fn format_tokens(n: u32) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}k", n / 1000)
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_widget() -> StatusBarWidget {
        StatusBarWidget {
            model: "gpt-4o".to_string(),
            context_percent: 42,
            state: StatusState::Idle,
            sprite_frame: 0,
            last_tick: Instant::now(),
            right_info: Some("main".to_string()),
            tokens_in: 0,
            tokens_out: 0,
            turn_elapsed: None,
        }
    }

    #[test]
    fn idle_renders_without_sprite() {
        let widget = make_widget();
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("gpt-4o"));
        assert!(text.contains("42%"));
        assert!(!text.contains("(~'.')~"));
    }

    #[test]
    fn thinking_shows_dance_sprite() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Thinking);
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(EVE_SPRITES[0]), "frame 0: {text:?}");
    }

    #[test]
    fn tool_running_shows_spinner() {
        let mut widget = make_widget();
        widget.set_state(StatusState::ToolRunning {
            tool_name: "shell".to_string(),
        });
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("shell"), "tool name: {text:?}");
    }

    #[test]
    fn streaming_shows_token_count() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Streaming { tokens: 123 });
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("123"), "tokens: {text:?}");
    }

    #[test]
    fn tick_advances_sprite_frame() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Thinking);
        widget.last_tick = Instant::now() - Duration::from_secs(1);
        let before = widget.sprite_frame;
        widget.tick();
        assert_eq!(widget.sprite_frame, before + 1);
    }

    #[test]
    fn tick_does_not_advance_when_idle() {
        let mut widget = make_widget();
        widget.last_tick = Instant::now() - Duration::from_secs(1);
        let before = widget.sprite_frame;
        widget.tick();
        assert_eq!(widget.sprite_frame, before);
    }

    #[test]
    fn format_tokens_compact() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1234), "1.2k");
        assert_eq!(format_tokens(12345), "12k");
    }

    #[test]
    fn is_animating_reflects_state() {
        let mut w = make_widget();
        assert!(!w.is_animating());
        w.set_state(StatusState::Thinking);
        assert!(w.is_animating());
    }

    #[test]
    fn right_info_shows_token_counts_when_present() {
        let mut widget = make_widget();
        widget.tokens_in = 5000;
        widget.tokens_out = 1200;
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("↑5.0k"), "in tokens: {text:?}");
        assert!(text.contains("↓1.2k"), "out tokens: {text:?}");
    }
}

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

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    symbols,
    text::Span,
};
use unicode_width::UnicodeWidthChar;

use crate::events::{AgentMode, StatusState};
use crate::widgets::braille_canvas::{BrailleCanvas, Pattern};

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
/// Sparkline bar color (dim lavender).
const SPARKLINE_COLOR: Color = Color::Rgb(120, 112, 148);
/// Active spinner/dance color.
const DANCE_COLOR: Color = Color::Rgb(180, 167, 214);
/// Shimmer highlight color (bright white/lavender).
const SHIMMER_HIGHLIGHT: Color = Color::Rgb(220, 215, 240);
/// Dim text color for shimmer base.
const DIM_TEXT: Color = Color::Rgb(100, 98, 108);

// ── Animation data ──────────────────────────────────────────────────────────

/// Four-frame Eve dance sprites for a smooth sway.
const EVE_SPRITES: [&str; 4] = ["(~'.')~", "~('.'~)", "(~'.')~", " ('.')>"];

/// Interval between dance frame advances (thinking sprite).
const DANCE_INTERVAL: Duration = Duration::from_millis(400);

/// Interval for shimmer animation (~30fps for smooth sweep).
const SHIMMER_INTERVAL: Duration = Duration::from_millis(33);

/// Maximum number of per-turn token measurements to keep.
const TOKEN_HISTORY_CAP: usize = 20;

/// Width of the sparkline in cells.
const SPARKLINE_WIDTH: usize = 12;

/// Idle tick interval for effects (~100ms).
const IDLE_EFFECTS_INTERVAL: Duration = Duration::from_millis(100);

/// Width of the inline braille heartbeat in cells.
const HEARTBEAT_WIDTH: usize = 10;

// ─────────────────────────────────────────────────────────────────────────────

/// A single-row status bar rendered at the bottom of the TUI viewport.
pub struct StatusBarWidget {
    /// Currently active model name.
    pub model: String,
    /// Context window usage percentage (0–100).
    pub context_percent: u8,
    /// Current agent state.
    pub state: StatusState,
    /// Previous state before the last `set_state` call.
    prev_state: Option<StatusState>,
    /// Current animation frame index (Eve dance sprite).
    pub sprite_frame: usize,
    /// When the last shimmer/animation tick happened.
    pub last_tick: Instant,
    /// When the last dance sprite frame advance happened.
    last_sprite_tick: Instant,
    /// Git branch name (or cwd fallback). `None` until first populated.
    right_info: Option<String>,
    /// Cumulative input tokens this session.
    pub tokens_in: u64,
    /// Cumulative output tokens this session.
    pub tokens_out: u64,
    /// Elapsed time since the current turn started.
    pub turn_elapsed: Option<Duration>,
    /// Per-turn token totals (input + output) for sparkline display.
    token_history: VecDeque<u64>,
    /// Whether effects are enabled (used for animation interval).
    effects_enabled: bool,
    /// Inline braille heartbeat pattern.
    heartbeat: Pattern,
    /// Current agent operating mode (Act / Plan).
    agent_mode: AgentMode,
    /// Shimmer phase (0.0..1.0), advances each tick for the cosine-wave sweep.
    shimmer_phase: f64,
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
            prev_state: None,
            sprite_frame: 0,
            last_tick: Instant::now(),
            last_sprite_tick: Instant::now(),
            right_info: None,
            tokens_in: 0,
            tokens_out: 0,
            turn_elapsed: None,
            token_history: VecDeque::with_capacity(TOKEN_HISTORY_CAP),
            effects_enabled: false,
            heartbeat: Pattern::Flatline,
            agent_mode: AgentMode::default(),
            shimmer_phase: 0.0,
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

    /// Update the current agent state, storing the previous for transition effects.
    pub fn set_state(&mut self, state: StatusState) {
        let was_idle = matches!(self.state, StatusState::Idle);
        let is_idle = matches!(state, StatusState::Idle);
        let old = std::mem::replace(&mut self.state, state);
        self.prev_state = Some(old);
        self.sprite_frame = 0;
        self.last_tick = Instant::now();
        self.last_sprite_tick = Instant::now();

        // Switch heartbeat pattern on state transitions.
        if was_idle && !is_idle {
            self.heartbeat = Pattern::Waveform {
                phase: 0.0,
                frequency: 2.0,
            };
        } else if !was_idle && is_idle {
            self.heartbeat = Pattern::Flatline;
        }
    }

    /// Returns the most recent state transition as `(from, to)`.
    ///
    /// Returns `None` if `set_state` has never been called.
    pub fn last_state_change(&self) -> Option<(&StatusState, &StatusState)> {
        self.prev_state.as_ref().map(|prev| (prev, &self.state))
    }

    /// Record per-turn token usage for sparkline display.
    ///
    /// Pushes `tokens_in + tokens_out` onto the history ring, evicting the
    /// oldest entry when the capacity limit is reached.
    pub fn record_turn_tokens(&mut self, tokens_in: u64, tokens_out: u64) {
        if self.token_history.len() >= TOKEN_HISTORY_CAP {
            self.token_history.pop_front();
        }
        self.token_history.push_back(tokens_in + tokens_out);
    }

    /// Set whether effects are enabled (affects idle animation interval).
    pub fn set_effects_enabled(&mut self, enabled: bool) {
        self.effects_enabled = enabled;
    }

    /// Update the displayed model name.
    pub fn set_model(&mut self, model: String) {
        self.model = model;
    }

    /// Update the context usage percentage.
    pub fn set_context_percent(&mut self, pct: u8) {
        self.context_percent = pct;
    }

    /// Update the agent operating mode (Act / Plan).
    pub fn set_agent_mode(&mut self, mode: AgentMode) {
        self.agent_mode = mode;
    }

    /// Whether the status bar is in an animated state (needs periodic redraws).
    pub fn is_animating(&self) -> bool {
        !matches!(self.state, StatusState::Idle)
    }

    /// The preferred animation interval for the current state.
    ///
    /// When effects are enabled and the agent is idle, returns ~100ms so
    /// idle ambient effects (border glow, breathing) get ticked.
    pub fn animation_interval(&self) -> Duration {
        match &self.state {
            StatusState::Thinking | StatusState::ToolRunning { .. } | StatusState::Streaming { .. } => {
                SHIMMER_INTERVAL
            }
            StatusState::Idle => {
                if self.effects_enabled {
                    IDLE_EFFECTS_INTERVAL
                } else {
                    Duration::from_secs(3600)
                }
            }
        }
    }

    /// Advance the animation frame if enough time has elapsed.
    pub fn tick(&mut self) {
        let now = Instant::now();
        // Tick the heartbeat with the actual elapsed time since last tick.
        let dt = now.duration_since(self.last_tick);
        self.heartbeat.tick(dt);

        if matches!(self.state, StatusState::Idle) {
            self.last_tick = now;
            return;
        }

        // Advance shimmer phase at SHIMMER_INTERVAL cadence (~30fps).
        if dt >= SHIMMER_INTERVAL {
            const SHIMMER_SPEED: f64 = 0.04; // ~2s for a full sweep at 33ms ticks
            self.shimmer_phase = (self.shimmer_phase + SHIMMER_SPEED) % 1.0;
            self.last_tick = now;
        }

        // Advance dance sprite frame at its own slower cadence.
        let sprite_dt = now.duration_since(self.last_sprite_tick);
        if sprite_dt >= DANCE_INTERVAL {
            self.sprite_frame = self.sprite_frame.wrapping_add(1);
            self.last_sprite_tick = now;
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

        // Heartbeat width: show if there's enough room (needs ~12 cells + padding).
        let heartbeat_w = if total > left_w + center_w + right_w + HEARTBEAT_WIDTH + 6 {
            HEARTBEAT_WIDTH + 1 // 1 cell gap
        } else {
            0
        };

        // Sparkline width: only show if we have data and enough room.
        let sparkline_w = if !self.token_history.is_empty() {
            // 2 cells padding + sparkline cells
            (SPARKLINE_WIDTH + 2).min(
                total.saturating_sub(left_w + center_w + right_w + heartbeat_w + 6),
            )
        } else {
            0
        };

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

            // Draw fill between center and heartbeat/sparkline/right.
            let center_end = area.x + center_start as u16 + center_w as u16 + 1;
            fill_to_decorations(center_end, area.x + right_start as u16, sparkline_w + heartbeat_w, row, bg, buf);
        } else {
            // No center — fill between left and heartbeat/sparkline/right.
            let fill_start = area.x + 1 + left_w as u16 + 1;
            fill_to_decorations(fill_start, area.x + right_start as u16, sparkline_w + heartbeat_w, row, bg, buf);
        }

        // Draw heartbeat (braille canvas, 1-row inline).
        if heartbeat_w > 0 {
            let hb_x = area.x + right_start as u16 - (sparkline_w + heartbeat_w) as u16;
            let hb_area = Rect {
                x: hb_x,
                y: row,
                width: HEARTBEAT_WIDTH as u16,
                height: 1,
            };
            BrailleCanvas::new(&self.heartbeat).render(hb_area, buf);
        }

        // Draw sparkline (if we have data).
        if sparkline_w > 0 {
            let spark_x = area.x + right_start as u16 - sparkline_w as u16 + 1;
            self.render_sparkline(spark_x, row, SPARKLINE_WIDTH, bg, buf);
        }

        // Draw right.
        write_spans(&right, area.x + right_start as u16, row, area.x + area.width, buf);
    }

    /// Render a tiny bar sparkline inline into the status bar.
    fn render_sparkline(&self, start_x: u16, row: u16, width: usize, bg: Color, buf: &mut Buffer) {
        let bar_set = symbols::bar::NINE_LEVELS;
        let data: Vec<u64> = self.token_history.iter().copied().collect();
        let max_val = data.iter().copied().max().unwrap_or(1).max(1);

        // Take the last `width` entries (or pad with zeros on the left).
        let style = Style::default().fg(SPARKLINE_COLOR).bg(bg);
        for i in 0..width {
            let data_idx = if data.len() >= width {
                i + data.len() - width
            } else if i >= width - data.len() {
                i - (width - data.len())
            } else {
                // No data for this column — render empty bar.
                let x = start_x + i as u16;
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_symbol(bar_set.empty);
                    cell.set_style(style);
                }
                continue;
            };

            let val = data[data_idx];
            // Map to 0..8 bar levels.
            let level = if max_val > 0 {
                ((val as f64 / max_val as f64) * 8.0).round() as usize
            } else {
                0
            };
            let sym = match level {
                0 => bar_set.empty,
                1 => bar_set.one_eighth,
                2 => bar_set.one_quarter,
                3 => bar_set.three_eighths,
                4 => bar_set.half,
                5 => bar_set.five_eighths,
                6 => bar_set.three_quarters,
                7 => bar_set.seven_eighths,
                _ => bar_set.full,
            };

            let x = start_x + i as u16;
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_symbol(sym);
                cell.set_style(style);
            }
        }
    }

    // ── Section builders ─────────────────────────────────────────────────

    fn build_left(&self, bg: Color) -> Vec<Span<'static>> {
        let mut spans = Vec::with_capacity(4);

        // Diamond accent.
        spans.push(Span::styled(
            "◆ ",
            Style::default().fg(ACCENT).bg(bg),
        ));

        // Mode indicator (Act / Plan).
        let mode_color = match self.agent_mode {
            AgentMode::Act => Color::Rgb(135, 175, 95),    // Green
            AgentMode::Plan => Color::Rgb(180, 167, 214),  // Lavender
        };
        spans.push(Span::styled(
            format!("[{}] ", self.agent_mode.label()),
            Style::default()
                .fg(mode_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
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
                let mut spans = vec![Span::styled(
                    format!(" {sprite} "),
                    Style::default().fg(DANCE_COLOR).bg(bg),
                )];
                let text = format!("thinking {elapsed}");
                spans.extend(shimmer_spans(&text, self.shimmer_phase, DIM_TEXT, SHIMMER_HIGHLIGHT, bg));
                spans
            }

            StatusState::ToolRunning { tool_name } => {
                let elapsed = self.format_elapsed();
                let text = format!(" {} {}", tool_name, elapsed);
                shimmer_spans(&text, self.shimmer_phase, DIM_TEXT, SHIMMER_HIGHLIGHT, bg)
            }

            StatusState::Streaming { tokens } => {
                let text = format!(" streaming {} tok ", tokens);
                shimmer_spans(&text, self.shimmer_phase, DIM_TEXT, SHIMMER_HIGHLIGHT, bg)
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

/// Fill with "─" from `fill_start` up to the decorations (heartbeat + sparkline) area.
fn fill_to_decorations(
    fill_start: u16,
    right_start: u16,
    decorations_w: usize,
    row: u16,
    bg: Color,
    buf: &mut Buffer,
) {
    let decorations_start = if decorations_w > 0 {
        right_start - decorations_w as u16
    } else {
        right_start
    };
    if fill_start < decorations_start {
        let fill_style = Style::default().fg(SEP_COLOR).bg(bg);
        for x in fill_start..decorations_start {
            if let Some(cell) = buf.cell_mut((x, row)) {
                cell.set_symbol("─");
                cell.set_style(fill_style);
            }
        }
    }
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
fn format_tokens(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else if n < 10_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}k", n / 1000)
    }
}

// ── Shimmer ─────────────────────────────────────────────────────────────────

/// Apply a shimmer effect to text — a cosine-wave color band sweeps across characters.
///
/// Based on the Codex CLI pattern: 2-second period, cosine interpolation.
fn shimmer_spans(
    text: &str,
    phase: f64,
    base_fg: Color,
    highlight_fg: Color,
    bg: Color,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![];
    }

    let padding = 10.0;
    let total_width = chars.len() as f64 + 2.0 * padding;
    let band_half_width = total_width / 4.0;

    // Phase maps to position in the sweep (0.0 = start, 1.0 = end).
    let center = phase * total_width - padding;

    chars
        .into_iter()
        .enumerate()
        .map(|(i, ch)| {
            let dist = (i as f64 - center).abs();
            let t = if dist < band_half_width {
                0.5 * (1.0 + (std::f64::consts::PI * dist / band_half_width).cos())
            } else {
                0.0
            };

            let fg = blend_color(base_fg, highlight_fg, (t * 0.9) as f32);
            Span::styled(ch.to_string(), Style::default().fg(fg).bg(bg))
        })
        .collect()
}

/// Linearly blend two RGB colors.
fn blend_color(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let r = (r1 as f32 + (r2 as f32 - r1 as f32) * t) as u8;
            let g = (g1 as f32 + (g2 as f32 - g1 as f32) * t) as u8;
            let b = (b1 as f32 + (b2 as f32 - b1 as f32) * t) as u8;
            Color::Rgb(r, g, b)
        }
        _ => {
            if t > 0.5 {
                to
            } else {
                from
            }
        }
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
            prev_state: None,
            sprite_frame: 0,
            last_tick: Instant::now(),
            last_sprite_tick: Instant::now(),
            right_info: Some("main".to_string()),
            tokens_in: 0,
            tokens_out: 0,
            turn_elapsed: None,
            token_history: VecDeque::new(),
            effects_enabled: false,
            heartbeat: Pattern::Flatline,
            agent_mode: AgentMode::default(),
            shimmer_phase: 0.0,
        }
    }

    // ── Snapshot tests ────────────────────────────────────────────────

    #[test]
    fn snapshot_idle_status_bar() {
        let widget = make_widget();
        let line = widget.to_line();
        let spans_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!(spans_text);
    }

    #[test]
    fn snapshot_thinking_status_bar() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Thinking);
        let line = widget.to_line();
        let spans_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!(spans_text);
    }

    #[test]
    fn snapshot_tool_running_status_bar() {
        let mut widget = make_widget();
        widget.set_state(StatusState::ToolRunning {
            tool_name: "shell_exec".to_string(),
        });
        let line = widget.to_line();
        let spans_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!(spans_text);
    }

    #[test]
    fn snapshot_streaming_status_bar() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Streaming { tokens: 256 });
        let line = widget.to_line();
        let spans_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!(spans_text);
    }

    #[test]
    fn snapshot_status_bar_with_tokens() {
        let mut widget = make_widget();
        widget.tokens_in = 5000;
        widget.tokens_out = 1200;
        let line = widget.to_line();
        let spans_text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!(spans_text);
    }

    // ── Non-snapshot tests ────────────────────────────────────────────

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
        widget.last_sprite_tick = Instant::now() - Duration::from_secs(1);
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

    #[test]
    fn record_turn_tokens_accumulates_correctly() {
        let mut w = make_widget();
        w.record_turn_tokens(100, 50);
        w.record_turn_tokens(200, 100);
        w.record_turn_tokens(300, 150);
        assert_eq!(w.token_history.len(), 3);
        assert_eq!(w.token_history[0], 150); // 100 + 50
        assert_eq!(w.token_history[1], 300); // 200 + 100
        assert_eq!(w.token_history[2], 450); // 300 + 150
    }

    #[test]
    fn record_turn_tokens_caps_at_20() {
        let mut w = make_widget();
        for i in 0..25 {
            w.record_turn_tokens(i * 10, i * 5);
        }
        assert_eq!(w.token_history.len(), TOKEN_HISTORY_CAP);
        // First entry should be from i=5 (evicted 0..5).
        assert_eq!(w.token_history[0], 5 * 10 + 5 * 5); // 75
    }

    #[test]
    fn last_state_change_returns_correct_transitions() {
        let mut w = make_widget();
        assert!(w.last_state_change().is_none(), "no transition yet");

        w.set_state(StatusState::Thinking);
        let (from, to) = w.last_state_change().unwrap();
        assert!(matches!(from, StatusState::Idle));
        assert!(matches!(to, StatusState::Thinking));

        w.set_state(StatusState::ToolRunning {
            tool_name: "shell".to_string(),
        });
        let (from, to) = w.last_state_change().unwrap();
        assert!(matches!(from, StatusState::Thinking));
        assert!(matches!(to, StatusState::ToolRunning { .. }));

        w.set_state(StatusState::Idle);
        let (from, to) = w.last_state_change().unwrap();
        assert!(matches!(from, StatusState::ToolRunning { .. }));
        assert!(matches!(to, StatusState::Idle));
    }

    #[test]
    fn animation_interval_returns_idle_effects_interval_when_effects_enabled() {
        let mut w = make_widget();
        // Without effects: 1 hour.
        assert_eq!(w.animation_interval(), Duration::from_secs(3600));

        // With effects enabled: ~100ms.
        w.set_effects_enabled(true);
        assert_eq!(w.animation_interval(), IDLE_EFFECTS_INTERVAL);

        // Non-idle states use shimmer interval.
        w.set_state(StatusState::Thinking);
        assert_eq!(w.animation_interval(), SHIMMER_INTERVAL);
    }

    #[test]
    fn heartbeat_changes_on_state_transition() {
        let mut w = make_widget();
        // Starts as Flatline.
        assert!(matches!(w.heartbeat, Pattern::Flatline));

        // Transition to active: becomes Waveform.
        w.set_state(StatusState::Thinking);
        assert!(
            matches!(w.heartbeat, Pattern::Waveform { .. }),
            "heartbeat should be Waveform when active"
        );

        // Transition between active states: stays Waveform.
        w.set_state(StatusState::ToolRunning {
            tool_name: "shell".to_string(),
        });
        assert!(
            matches!(w.heartbeat, Pattern::Waveform { .. }),
            "heartbeat should stay Waveform between active states"
        );

        // Transition back to idle: becomes Flatline.
        w.set_state(StatusState::Idle);
        assert!(
            matches!(w.heartbeat, Pattern::Flatline),
            "heartbeat should be Flatline when idle"
        );
    }

    #[test]
    fn shimmer_produces_per_char_spans() {
        let spans = shimmer_spans(
            "hello",
            0.5,
            Color::Rgb(50, 50, 50),
            Color::Rgb(200, 200, 200),
            Color::Rgb(0, 0, 0),
        );
        assert_eq!(spans.len(), 5); // one per character
    }

    #[test]
    fn shimmer_empty_text() {
        let spans = shimmer_spans(
            "",
            0.0,
            Color::Rgb(50, 50, 50),
            Color::Rgb(200, 200, 200),
            Color::Rgb(0, 0, 0),
        );
        assert!(spans.is_empty());
    }

    #[test]
    fn blend_color_midpoint() {
        let c = blend_color(Color::Rgb(0, 0, 0), Color::Rgb(200, 200, 200), 0.5);
        assert!(matches!(c, Color::Rgb(r, g, b) if r == 100 && g == 100 && b == 100));
    }

    #[test]
    fn blend_color_endpoints() {
        let from = Color::Rgb(10, 20, 30);
        let to = Color::Rgb(110, 120, 130);
        assert_eq!(blend_color(from, to, 0.0), from);
        assert_eq!(blend_color(from, to, 1.0), to);
    }

    #[test]
    fn shimmer_phase_advances_in_tick() {
        let mut widget = make_widget();
        widget.state = StatusState::Thinking;
        widget.last_tick = Instant::now() - Duration::from_millis(100);
        let old_phase = widget.shimmer_phase;
        widget.tick();
        assert!(widget.shimmer_phase > old_phase, "shimmer should advance");
    }
}

//! Status bar widget — a single-row footer showing model info, context usage,
//! and an Eve dance sprite or spinner during active operations.
//!
//! Layout (always 1 row, full width):
//!   [left: model · context%]   [center: sprite/spinner]   [right: cwd/branch]

use std::time::{Duration, Instant};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
};

use crate::events::StatusState;

// ── Color constants (mirrored from genesis_ui::colors to avoid a dep cycle) ──

/// Dim gray for peripheral info text.
const UI_DIM: Color = Color::Rgb(108, 108, 108);
/// Lavender for Eve dance sprite.
const EVE_LAVENDER: Color = Color::Rgb(180, 167, 214);

// ── Animation data ────────────────────────────────────────────────────────────

/// Two-frame Eve dance sprites.
const EVE_SPRITES: [&str; 2] = ["(~'.')~", "~('.'~)"];

/// Ten-frame Braille spinner.
const SPINNER_FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Interval between sprite frame advances for the Thinking dance.
const DANCE_INTERVAL: Duration = Duration::from_millis(500);

/// Interval between spinner frame advances for ToolRunning.
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);

// ─────────────────────────────────────────────────────────────────────────────

/// A single-row status bar rendered as the last row of the TUI viewport.
pub struct StatusBarWidget {
    /// Currently active model name, e.g. `"gpt-4o"`.
    pub model: String,
    /// Context window usage percentage (0–100).
    pub context_percent: u8,
    /// Current agent state.
    pub state: StatusState,
    /// Current animation frame index (for both sprite and spinner).
    pub sprite_frame: usize,
    /// When the last frame advance happened.
    pub last_tick: Instant,
    /// Right-side info string (git branch or cwd).
    right_info: String,
}

impl StatusBarWidget {
    /// Create a new status bar with the given model name.
    ///
    /// The right-side info is initialised from `git rev-parse --abbrev-ref HEAD`
    /// if available, falling back to the current working directory.
    pub fn new(model: String) -> Self {
        let right_info = Self::detect_right_info();
        Self {
            model,
            context_percent: 0,
            state: StatusState::Idle,
            sprite_frame: 0,
            last_tick: Instant::now(),
            right_info,
        }
    }

    /// Update the current agent state.
    pub fn set_state(&mut self, state: StatusState) {
        self.state = state;
        // Reset the frame counter so transitions start from a clean frame.
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

    /// Advance the animation frame if enough time has elapsed.
    ///
    /// Should be called on every TUI draw tick.
    pub fn tick(&mut self) {
        let interval = match &self.state {
            StatusState::Thinking => DANCE_INTERVAL,
            StatusState::ToolRunning { .. } => SPINNER_INTERVAL,
            StatusState::Streaming { .. } => SPINNER_INTERVAL,
            StatusState::Idle => return,
        };

        if self.last_tick.elapsed() >= interval {
            self.sprite_frame = self.sprite_frame.wrapping_add(1);
            self.last_tick = Instant::now();
        }
    }

    /// Render the status bar into `buf` within `area`.
    ///
    /// Only the first row of `area` is used.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        // Use only the first row.
        let row = area.y;

        let left = self.build_left();
        let center = self.build_center();
        let right = self.build_right();

        // Visible widths (character counts, not byte counts).
        let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
        let center_width: usize = center.iter().map(|s| s.content.chars().count()).sum();
        let right_width: usize = right.iter().map(|s| s.content.chars().count()).sum();

        let total_width = area.width as usize;

        // Centre the center section.
        let center_start = if total_width > center_width {
            total_width / 2 - center_width / 2
        } else {
            left_width + 1
        };

        // Right-align the right section.
        let right_start = if total_width > right_width + 1 {
            total_width - right_width - 1
        } else {
            total_width.saturating_sub(right_width)
        };

        // Fill the row with background spaces first.
        let dim_style = Style::default().fg(UI_DIM);
        for x in area.x..area.x + area.width {
            let cell = buf.cell_mut((x, row)).expect("cell in area");
            cell.set_char(' ');
            cell.set_style(dim_style);
        }

        // Write left section.
        let mut x = area.x + 1;
        for span in &left {
            for ch in span.content.chars() {
                if x >= area.x + area.width {
                    break;
                }
                let cell = buf.cell_mut((x, row)).expect("cell in area");
                cell.set_char(ch);
                cell.set_style(span.style);
                x += 1;
            }
        }

        // Write center section.
        let mut x = area.x + center_start as u16;
        for span in &center {
            for ch in span.content.chars() {
                if x >= area.x + area.width {
                    break;
                }
                let cell = buf.cell_mut((x, row)).expect("cell in area");
                cell.set_char(ch);
                cell.set_style(span.style);
                x += 1;
            }
        }

        // Write right section.
        let mut x = area.x + right_start as u16;
        for span in &right {
            for ch in span.content.chars() {
                if x >= area.x + area.width {
                    break;
                }
                let cell = buf.cell_mut((x, row)).expect("cell in area");
                cell.set_char(ch);
                cell.set_style(span.style);
                x += 1;
            }
        }
    }

    // ── Private helpers ───────────────────────────────────────────────────

    fn build_left(&self) -> Vec<Span<'static>> {
        let text = format!("{} · {}%", self.model, self.context_percent);
        vec![Span::styled(text, Style::default().fg(UI_DIM))]
    }

    fn build_center(&self) -> Vec<Span<'static>> {
        match &self.state {
            StatusState::Idle => vec![],

            StatusState::Thinking => {
                let frame = self.sprite_frame % EVE_SPRITES.len();
                let sprite = EVE_SPRITES[frame];
                vec![Span::styled(
                    sprite.to_string(),
                    Style::default().fg(EVE_LAVENDER),
                )]
            }

            StatusState::ToolRunning { tool_name } => {
                let spinner = SPINNER_FRAMES[self.sprite_frame % SPINNER_FRAMES.len()];
                vec![
                    Span::styled(
                        format!("{spinner} "),
                        Style::default().fg(UI_DIM),
                    ),
                    Span::styled(
                        tool_name.clone(),
                        Style::default().fg(UI_DIM),
                    ),
                ]
            }

            StatusState::Streaming { tokens } => {
                vec![Span::styled(
                    format!("... {tokens} tokens"),
                    Style::default().fg(UI_DIM),
                )]
            }
        }
    }

    fn build_right(&self) -> Vec<Span<'static>> {
        vec![Span::styled(
            self.right_info.clone(),
            Style::default().fg(UI_DIM),
        )]
    }

    /// Detect the git branch, falling back to the current working directory.
    fn detect_right_info() -> String {
        // Try git rev-parse
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

        // Fall back to cwd.
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "~".to_string())
    }
}

// ── Convenience conversion for building a `Line` from the status bar ─────────

impl StatusBarWidget {
    /// Render to a ratatui `Line` for use in tests or snapshot inspection.
    ///
    /// Not used in the main render path; useful for unit-testing content.
    pub fn to_line(&self) -> Line<'static> {
        let mut spans = Vec::new();
        spans.extend(self.build_left());
        let center = self.build_center();
        if !center.is_empty() {
            spans.push(Span::raw("  "));
            spans.extend(center);
        }
        let right = self.build_right();
        if !right.is_empty() {
            spans.push(Span::raw("  "));
            spans.extend(right);
        }
        Line::from(spans)
    }
}

// ─────────────────────────────────────────────────────────────────────────────

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
            right_info: "main".to_string(),
        }
    }

    #[test]
    fn idle_renders_without_sprite() {
        let widget = make_widget();
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Idle should show model + context but NO sprite characters.
        assert!(text.contains("gpt-4o"));
        assert!(text.contains("42%"));
        assert!(!text.contains("(~'.')~"));
        assert!(!text.contains("~('.'~)"));
    }

    #[test]
    fn thinking_shows_dance_sprite() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Thinking);

        // Frame 0 → first sprite.
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(EVE_SPRITES[0]), "frame 0 should show first sprite");

        // Advance one frame manually.
        widget.sprite_frame = 1;
        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(EVE_SPRITES[1]), "frame 1 should show second sprite");
    }

    #[test]
    fn tool_running_shows_spinner() {
        let mut widget = make_widget();
        widget.set_state(StatusState::ToolRunning {
            tool_name: "shell".to_string(),
        });

        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        // Should contain the spinner char and the tool name.
        let spinner_char = SPINNER_FRAMES[0];
        assert!(
            text.contains(spinner_char),
            "spinner char should be present; got: {text:?}"
        );
        assert!(text.contains("shell"), "tool name should be present");
    }

    #[test]
    fn streaming_shows_token_count() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Streaming { tokens: 123 });

        let line = widget.to_line();
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("123 tokens"), "token count should be displayed; got: {text:?}");
    }

    #[test]
    fn tick_advances_sprite_frame() {
        let mut widget = make_widget();
        widget.set_state(StatusState::Thinking);
        // Force last_tick to be old enough.
        widget.last_tick = Instant::now() - Duration::from_secs(1);
        let initial_frame = widget.sprite_frame;
        widget.tick();
        assert_eq!(
            widget.sprite_frame,
            initial_frame + 1,
            "tick should advance sprite_frame"
        );
    }

    #[test]
    fn tick_does_not_advance_when_idle() {
        let mut widget = make_widget();
        // state is Idle by default.
        widget.last_tick = Instant::now() - Duration::from_secs(1);
        let initial_frame = widget.sprite_frame;
        widget.tick();
        assert_eq!(widget.sprite_frame, initial_frame, "idle tick should not advance frame");
    }
}

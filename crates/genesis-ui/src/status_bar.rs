//! Active-only status bar rendered on the terminal's last row.
//!
//! The bar appears only during active operations (thinking, streaming, tool
//! execution) and disappears when idle. It shows elapsed time, a dance-frame
//! animation, token counts, and estimated cost.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crossterm::{cursor, terminal, ExecutableCommand};

use crate::colors::*;

/// What the agent is currently doing.
#[derive(Debug, Clone, PartialEq)]
pub enum BarState {
    /// LLM is generating (no tokens received yet).
    Thinking,
    /// Tokens are actively being streamed to the user.
    Streaming,
    /// A single tool is executing.
    ToolRunning { name: String },
    /// Multiple tools executing in parallel.
    ToolGroup { current: usize, total: usize },
    /// An error just occurred.
    Error,
}

/// A status bar that is drawn only while the agent is active.
pub struct StatusBar {
    state: Option<BarState>,
    session_id: String,
    tokens_in: u64,
    tokens_out: u64,
    cost: f64,
    started_at: Option<Instant>,
    dance_frame: usize,
    last_draw: Option<Instant>,
    colors_enabled: bool,
}

impl StatusBar {
    /// Create a new status bar for the given session.
    pub fn new(session_id: &str, colors_enabled: bool) -> Self {
        Self {
            state: None,
            session_id: session_id.to_string(),
            tokens_in: 0,
            tokens_out: 0,
            cost: 0.0,
            started_at: None,
            dance_frame: 0,
            last_draw: None,
            colors_enabled,
        }
    }

    /// Current state (if any).
    pub fn state(&self) -> Option<&BarState> {
        self.state.as_ref()
    }

    /// Current dance frame index.
    pub fn dance_frame(&self) -> usize {
        self.dance_frame
    }

    /// Transition to a new active state and redraw.
    pub fn set_state(&mut self, state: BarState) {
        self.state = Some(state);
        if self.started_at.is_none() {
            self.started_at = Some(Instant::now());
        }
        self.draw();
    }

    /// Clear the bar and return to idle (no visible bar).
    pub fn clear(&mut self) {
        if self.state.is_some() {
            self.clear_line();
        }
        self.state = None;
        self.started_at = None;
        self.dance_frame = 0;
    }

    /// Update token/cost counters (does not trigger a redraw by itself).
    pub fn update_tokens(&mut self, tokens_in: u64, tokens_out: u64, cost: f64) {
        self.tokens_in = tokens_in;
        self.tokens_out = tokens_out;
        self.cost = cost;
    }

    /// Advance animation frame and redraw (throttled to ~100 ms).
    pub fn tick(&mut self) {
        if self.state.is_none() {
            return;
        }
        // Throttle redraws
        if let Some(last) = self.last_draw {
            if last.elapsed() < Duration::from_millis(100) {
                return;
            }
        }
        self.dance_frame = (self.dance_frame + 1) % 3;
        self.draw();
    }

    // ── Internal helpers ─────────────────────────────────────────────

    fn draw(&mut self) {
        if !self.colors_enabled || self.state.is_none() {
            return;
        }
        let Some(ref state) = self.state else {
            return;
        };
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let cols = cols as usize;

        let elapsed = self.started_at.map(|s| s.elapsed()).unwrap_or_default();
        let elapsed_str = if elapsed.as_secs() >= 60 {
            format!(
                "({}m{}s)",
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60
            )
        } else {
            format!("({:.1}s)", elapsed.as_secs_f64())
        };

        let state_text = match state {
            BarState::Thinking => "thinking...".to_string(),
            BarState::Streaming => "streaming...".to_string(),
            BarState::ToolRunning { name } => format!("{name}..."),
            BarState::ToolGroup { current, total } => {
                format!("running tools {current}/{total}")
            }
            BarState::Error => "error".to_string(),
        };

        let dance = match state {
            BarState::Thinking | BarState::ToolRunning { .. } => {
                match self.dance_frame % 3 {
                    0 => " (~'.')~",
                    1 => " ~('.'~)",
                    _ => " (~'.')~",
                }
            }
            _ => "",
        };

        let session_short = if self.session_id.len() > 8 {
            &self.session_id[..8]
        } else {
            &self.session_id
        };

        // ANSI escape sequences for color
        let bg = format!(
            "\x1b[48;2;{};{};{}m",
            UI_MUTED.0, UI_MUTED.1, UI_MUTED.2
        );
        let accent = format!(
            "\x1b[38;2;{};{};{}m",
            UI_ACCENT.0, UI_ACCENT.1, UI_ACCENT.2
        );
        let text_c = format!(
            "\x1b[38;2;{};{};{}m",
            UI_TEXT.0, UI_TEXT.1, UI_TEXT.2
        );
        let dim = format!(
            "\x1b[38;2;{};{};{}m",
            UI_DIM.0, UI_DIM.1, UI_DIM.2
        );

        let left = format!(
            "{accent}\u{2666} {text_c}{state_text}  {dim}{elapsed_str}{text_c}{dance}"
        );
        let right = format!(
            "{dim}{session_short}  {}\u{2191} {}\u{2193}  ${:.2}",
            self.tokens_in, self.tokens_out, self.cost
        );

        // Calculate visible widths (excluding ANSI codes) for padding
        let left_visible = format!("\u{2666} {state_text}  {elapsed_str}{dance}");
        let right_visible = format!(
            "{session_short}  {}\u{2191} {}\u{2193}  ${:.2}",
            self.tokens_in, self.tokens_out, self.cost
        );
        // +2 accounts for the leading and trailing spaces
        let visible_len = left_visible.len() + right_visible.len() + 2;
        let padding = if cols > visible_len {
            " ".repeat(cols - visible_len)
        } else {
            " ".to_string()
        };

        let bar = format!("{bg} {left}{padding}{right} \x1b[0m");

        let mut stdout = io::stdout();
        let _ = stdout.execute(cursor::SavePosition);
        let _ = stdout.execute(cursor::MoveTo(0, rows - 1));
        let _ = stdout.execute(terminal::Clear(terminal::ClearType::CurrentLine));
        let _ = write!(stdout, "{bar}");
        let _ = stdout.execute(cursor::RestorePosition);
        let _ = stdout.flush();
        self.last_draw = Some(Instant::now());
    }

    fn clear_line(&self) {
        if !self.colors_enabled {
            return;
        }
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        let mut stdout = io::stdout();
        let _ = stdout.execute(cursor::SavePosition);
        let _ = stdout.execute(cursor::MoveTo(0, rows - 1));
        let _ = stdout.execute(terminal::Clear(terminal::ClearType::CurrentLine));
        let _ = stdout.execute(cursor::RestorePosition);
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_no_state() {
        let bar = StatusBar::new("abc123", false);
        assert!(bar.state().is_none());
    }

    #[test]
    fn set_state_changes_state() {
        let mut bar = StatusBar::new("abc123", false);
        bar.set_state(BarState::Thinking);
        assert_eq!(bar.state(), Some(&BarState::Thinking));

        bar.set_state(BarState::Streaming);
        assert_eq!(bar.state(), Some(&BarState::Streaming));

        bar.set_state(BarState::ToolRunning {
            name: "shell".to_string(),
        });
        assert_eq!(
            bar.state(),
            Some(&BarState::ToolRunning {
                name: "shell".to_string()
            })
        );
    }

    #[test]
    fn clear_resets_state() {
        let mut bar = StatusBar::new("abc123", false);
        bar.set_state(BarState::Thinking);
        assert!(bar.state().is_some());

        bar.clear();
        assert!(bar.state().is_none());
    }

    #[test]
    fn update_tokens_stores_values() {
        let mut bar = StatusBar::new("abc123", false);
        bar.update_tokens(100, 200, 0.05);
        assert_eq!(bar.tokens_in, 100);
        assert_eq!(bar.tokens_out, 200);
        assert!((bar.cost - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn dance_frame_cycles() {
        let mut bar = StatusBar::new("abc123", false);
        bar.set_state(BarState::Thinking);
        assert_eq!(bar.dance_frame(), 0);

        // Force past throttle by clearing last_draw
        bar.last_draw = None;
        bar.tick();
        assert_eq!(bar.dance_frame(), 1);

        bar.last_draw = None;
        bar.tick();
        assert_eq!(bar.dance_frame(), 2);

        bar.last_draw = None;
        bar.tick();
        assert_eq!(bar.dance_frame(), 0); // wraps around
    }

    #[test]
    fn tick_noop_when_idle() {
        let mut bar = StatusBar::new("abc123", false);
        // No state set — tick should be a no-op
        bar.tick();
        assert_eq!(bar.dance_frame(), 0);
    }

    #[test]
    fn clear_resets_dance_frame() {
        let mut bar = StatusBar::new("abc123", false);
        bar.set_state(BarState::Thinking);
        bar.last_draw = None;
        bar.tick();
        assert_eq!(bar.dance_frame(), 1);

        bar.clear();
        assert_eq!(bar.dance_frame(), 0);
    }
}

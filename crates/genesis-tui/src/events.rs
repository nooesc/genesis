//! Event types for the TUI event loop.
//!
//! Three streams: TuiEvent (terminal), AgentEvent (agent loop), AppEvent (internal).

use crossterm::event::KeyEvent;
use std::time::Duration;

/// Events from the terminal (keyboard, paste, resize, draw timer).
#[derive(Debug)]
pub enum TuiEvent {
    /// Keyboard input.
    Key(KeyEvent),
    /// Bracketed paste content.
    Paste(String),
    /// Terminal resized.
    Resize { width: u16, height: u16 },
    /// Frame timer tick — time to redraw.
    Draw,
    /// Terminal gained focus.
    FocusGained,
    /// Terminal lost focus.
    FocusLost,
}

/// Events from the agent loop (streaming responses, tool calls).
#[derive(Debug)]
pub enum AgentEvent {
    /// New turn has started.
    TurnStarted,
    /// Streaming text chunk from LLM.
    TextDelta(String),
    /// Tool execution starting.
    ToolCallStart {
        call_id: String,
        tool_name: String,
        args_summary: String,
    },
    /// Tool execution completed.
    ToolCallEnd {
        call_id: String,
        success: bool,
        duration: Duration,
    },
    /// Agent turn completed.
    TurnComplete {
        response: String,
        input_tokens: u64,
        output_tokens: u64,
        turns_used: usize,
        tool_calls_made: usize,
    },
    /// Agent needs clarification from user.
    ClarificationNeeded(String),
    /// The running turn was cancelled by the user (Ctrl+C).
    Cancelled,
    /// Agent encountered an error.
    Error(String),
    /// Non-fatal warning from agent.
    Warning(String),
}

/// Internal TUI events (state changes triggered by the app itself).
#[derive(Debug)]
pub enum AppEvent {
    /// Signal that a completed turn should be committed.
    CommitHistory,
    /// Update status bar state.
    UpdateStatus(StatusState),
    /// Show a fullscreen overlay (transcript, diff).
    ShowOverlay(OverlayKind),
    /// Close the current overlay.
    CloseOverlay,
    /// A slash command was entered.
    SlashCommand(String),
    /// Model was changed via /model.
    ModelChanged(String),
    /// Trigger async model list fetch for the model picker.
    FetchModels,
    /// Model list fetched — deliver to the picker overlay.
    ModelsFetched(Result<Vec<genesis_provider::openrouter_models::OpenRouterModel>, String>),
}

/// Status bar states.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusState {
    Idle,
    Thinking,
    Streaming { tokens: u64 },
    ToolRunning { tool_name: String },
}

/// Overlay kinds for fullscreen views.
#[derive(Debug)]
pub enum OverlayKind {
    Transcript,
    Help,
}

/// Agent operating mode — controls which tools are available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AgentMode {
    /// Full autonomy — all tools available.
    #[default]
    Act,
    /// Planning only — restricted to read-only tools.
    Plan,
}

impl AgentMode {
    pub fn toggle(self) -> Self {
        match self {
            AgentMode::Act => AgentMode::Plan,
            AgentMode::Plan => AgentMode::Act,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentMode::Act => "Act",
            AgentMode::Plan => "Plan",
        }
    }

    pub fn is_plan(self) -> bool {
        matches!(self, AgentMode::Plan)
    }
}

/// User actions sent to the agent.
#[derive(Debug)]
pub enum Submission {
    /// Send a message to the agent.
    UserMessage { text: String, images: Vec<String> },
    /// Interrupt the current turn (Ctrl+C).
    ///
    /// NOTE: This is only received when no turn future is active (used for
    /// cleanup). For immediate cancellation during a running turn, use the
    /// dedicated `cancel_tx` channel instead.
    Interrupt,
    /// Switch the active model (from model picker).
    /// Format: "backend/model" (e.g. "anthropic/claude-sonnet-4-6").
    ModelSwitch(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_mode_default_is_act() {
        assert_eq!(AgentMode::default(), AgentMode::Act);
    }

    #[test]
    fn agent_mode_toggles() {
        assert_eq!(AgentMode::Act.toggle(), AgentMode::Plan);
        assert_eq!(AgentMode::Plan.toggle(), AgentMode::Act);
    }

    #[test]
    fn agent_mode_labels() {
        assert_eq!(AgentMode::Act.label(), "Act");
        assert_eq!(AgentMode::Plan.label(), "Plan");
    }

    #[test]
    fn agent_mode_is_plan() {
        assert!(!AgentMode::Act.is_plan());
        assert!(AgentMode::Plan.is_plan());
    }
}

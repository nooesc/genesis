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
}

/// Status bar states.
#[derive(Debug)]
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

/// User actions sent to the agent.
#[derive(Debug)]
pub enum Submission {
    /// Send a message to the agent.
    UserMessage {
        text: String,
        images: Vec<String>,
    },
    /// Interrupt the current turn (Ctrl+C).
    Interrupt,
}

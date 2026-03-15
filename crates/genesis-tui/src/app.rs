//! Application state and event dispatch.

use crate::events::{AgentEvent, AppEvent, StatusState, Submission, TuiEvent};
use crate::frame_requester::FrameRequester;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

/// Central application state for the TUI event loop.
///
/// Owns the channel senders and tracks whether an agent turn is in flight.
/// Each `handle_*` method processes one event category, mutating state and
/// forwarding derived events to the appropriate channel.
pub struct App {
    pub submission_tx: mpsc::UnboundedSender<Submission>,
    pub app_tx: mpsc::UnboundedSender<AppEvent>,
    pub frame_requester: FrameRequester,
    pub turn_running: bool,
    pub should_exit: bool,
}

impl App {
    /// Process a terminal event (keyboard, paste, resize, etc.).
    pub fn handle_tui_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Key(key) => self.handle_key(key),
            TuiEvent::Paste(_text) => {
                // TODO(Task 11): insert into InputWidget
            }
            TuiEvent::Resize { .. } => self.frame_requester.schedule_frame(),
            TuiEvent::Draw | TuiEvent::FocusGained | TuiEvent::FocusLost => {}
        }
    }

    /// Process an event from the agent streaming callback.
    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted => {
                self.turn_running = true;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Thinking));
            }
            AgentEvent::TextDelta(_text) => {
                // TODO(Task 12): append to active cell in ChatWidget
            }
            AgentEvent::ToolCallStart { tool_name, .. } => {
                let _ = self
                    .app_tx
                    .send(AppEvent::UpdateStatus(StatusState::ToolRunning { tool_name }));
            }
            AgentEvent::ToolCallEnd { .. } => {}
            AgentEvent::TurnComplete { .. } => {
                self.turn_running = false;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
                let _ = self.app_tx.send(AppEvent::CommitHistory);
            }
            AgentEvent::ClarificationNeeded(_) => {}
            AgentEvent::Error(_err) => {
                self.turn_running = false;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
            }
            AgentEvent::Warning(_) => {}
        }
        self.frame_requester.schedule_frame();
    }

    /// Process an internal application event.
    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::CommitHistory => {
                // TODO(Task 15): push cells to scrollback
            }
            AppEvent::UpdateStatus(_state) => {
                // TODO(Task 21): update status bar widget
            }
            AppEvent::ShowOverlay(_kind) => {
                // TODO(Task 24): enter alt screen + overlay
            }
            AppEvent::CloseOverlay => {
                // TODO(Task 24): leave alt screen
            }
            AppEvent::SlashCommand(cmd) => match cmd.as_str() {
                "/exit" | "/quit" => self.should_exit = true,
                _ => {} // TODO(Task 23): handle other commands
            },
            AppEvent::ModelChanged(_) => {}
        }
    }

    /// Route a single key event to the appropriate handler.
    fn handle_key(&mut self, key: KeyEvent) {
        match (key.code, key.modifiers) {
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.should_exit = true;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.turn_running {
                    let _ = self.submission_tx.send(Submission::Interrupt);
                }
            }
            _ => {
                // TODO(Task 11): route to InputWidget
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    fn make_app() -> (
        App,
        mpsc::UnboundedReceiver<Submission>,
        mpsc::UnboundedReceiver<AppEvent>,
    ) {
        let (submission_tx, submission_rx) = mpsc::unbounded_channel();
        let (app_tx, app_rx) = mpsc::unbounded_channel();
        let (draw_tx, _draw_rx) = broadcast::channel(16);
        let frame_requester = FrameRequester::new(draw_tx);
        let app = App {
            submission_tx,
            app_tx,
            frame_requester,
            turn_running: false,
            should_exit: false,
        };
        (app, submission_rx, app_rx)
    }

    #[tokio::test]
    async fn ctrl_d_sets_should_exit() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        app.handle_tui_event(TuiEvent::Key(key));
        assert!(app.should_exit);
    }

    #[tokio::test]
    async fn ctrl_c_sends_interrupt_when_running() {
        let (mut app, mut sub_rx, _app_rx) = make_app();
        app.turn_running = true;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_tui_event(TuiEvent::Key(key));
        match sub_rx.try_recv() {
            Ok(Submission::Interrupt) => {}
            other => panic!("expected Submission::Interrupt, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn ctrl_c_does_nothing_when_idle() {
        let (mut app, mut sub_rx, _app_rx) = make_app();
        app.turn_running = false;
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.handle_tui_event(TuiEvent::Key(key));
        assert!(sub_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn turn_started_sets_thinking_status() {
        let (mut app, _sub_rx, mut app_rx) = make_app();
        app.handle_agent_event(AgentEvent::TurnStarted);
        assert!(app.turn_running);
        match app_rx.try_recv() {
            Ok(AppEvent::UpdateStatus(StatusState::Thinking)) => {}
            other => panic!("expected UpdateStatus(Thinking), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn turn_complete_resets_to_idle() {
        let (mut app, _sub_rx, mut app_rx) = make_app();
        app.turn_running = true;
        app.handle_agent_event(AgentEvent::TurnComplete {
            response: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            turns_used: 1,
            tool_calls_made: 0,
        });
        assert!(!app.turn_running);
        // First event: UpdateStatus(Idle)
        match app_rx.try_recv() {
            Ok(AppEvent::UpdateStatus(StatusState::Idle)) => {}
            other => panic!("expected UpdateStatus(Idle), got {:?}", other),
        }
        // Second event: CommitHistory
        match app_rx.try_recv() {
            Ok(AppEvent::CommitHistory) => {}
            other => panic!("expected CommitHistory, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn error_resets_turn_running() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.turn_running = true;
        app.handle_agent_event(AgentEvent::Error("boom".into()));
        assert!(!app.turn_running);
    }

    #[tokio::test]
    async fn slash_exit_sets_should_exit() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.handle_app_event(AppEvent::SlashCommand("/exit".into()));
        assert!(app.should_exit);
    }

    #[tokio::test]
    async fn slash_quit_sets_should_exit() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.handle_app_event(AppEvent::SlashCommand("/quit".into()));
        assert!(app.should_exit);
    }

    #[tokio::test]
    async fn tool_running_updates_status() {
        let (mut app, _sub_rx, mut app_rx) = make_app();
        app.handle_agent_event(AgentEvent::ToolCallStart {
            call_id: "call_1".into(),
            tool_name: "shell".into(),
            args_summary: "ls".into(),
        });
        match app_rx.try_recv() {
            Ok(AppEvent::UpdateStatus(StatusState::ToolRunning { tool_name })) => {
                assert_eq!(tool_name, "shell");
            }
            other => panic!("expected UpdateStatus(ToolRunning), got {:?}", other),
        }
    }
}

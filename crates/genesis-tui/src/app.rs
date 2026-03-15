//! Application state and event dispatch.

use crate::events::{AgentEvent, AppEvent, OverlayKind, StatusState, Submission, TuiEvent};
use crate::frame_requester::FrameRequester;
use crate::widgets::chat_widget::ChatWidget;
use crate::widgets::command_popup::{CommandAction, CommandPopup};
use crate::widgets::input_widget::InputAction;
use crate::widgets::status_bar::StatusBarWidget;
use crate::widgets::transcript::{TranscriptAction, TranscriptOverlay};
use crate::widgets::welcome::WelcomeWidget;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

/// Which top-level screen is currently displayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppScreen {
    /// The welcome/splash screen shown on startup.
    Welcome,
    /// The interactive chat screen.
    Chat,
}

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
    /// Which screen is currently active.
    pub screen: AppScreen,
    /// The welcome screen widget.
    pub welcome: WelcomeWidget,
    /// Composed chat area: history cells + active streaming cell + input.
    pub chat: ChatWidget,
    /// Single-row status bar rendered at the bottom of the viewport.
    pub status_bar: StatusBarWidget,
    /// Active fullscreen overlay, if any.
    pub overlay: Option<TranscriptOverlay>,
    /// Last known viewport height (used to pass visible_rows to the overlay).
    pub viewport_height: u16,
    /// Slash command popup (shown when input starts with `/`).
    pub command_popup: CommandPopup,
}

impl App {
    /// Process a terminal event (keyboard, paste, resize, etc.).
    pub fn handle_tui_event(&mut self, event: TuiEvent) {
        match event {
            TuiEvent::Key(key) => self.handle_key(key),
            TuiEvent::Paste(text) => {
                // Paste is ignored while an overlay is active.
                if self.overlay.is_none() {
                    self.chat.input.handle_paste(&text);
                }
                self.frame_requester.schedule_frame();
            }
            TuiEvent::Resize { height, .. } => {
                self.viewport_height = height;
                self.frame_requester.schedule_frame();
            }
            TuiEvent::Draw | TuiEvent::FocusGained | TuiEvent::FocusLost => {}
        }
    }

    /// Process an event from the agent streaming callback.
    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted => {
                self.turn_running = true;
                self.chat.start_turn();
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Thinking));
            }
            AgentEvent::TextDelta(text) => {
                self.chat.append_text(&text);
            }
            AgentEvent::ToolCallStart {
                call_id,
                tool_name,
                args_summary,
            } => {
                self.chat
                    .tool_call_start(call_id, tool_name.clone(), args_summary);
                let _ = self
                    .app_tx
                    .send(AppEvent::UpdateStatus(StatusState::ToolRunning { tool_name }));
            }
            AgentEvent::ToolCallEnd {
                call_id,
                success,
                duration,
            } => {
                self.chat.tool_call_end(&call_id, success, duration);
            }
            AgentEvent::TurnComplete { .. } => {
                self.chat.complete_turn();
                self.turn_running = false;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
                let _ = self.app_tx.send(AppEvent::CommitHistory);
            }
            AgentEvent::ClarificationNeeded(_) => {}
            AgentEvent::Error(_err) => {
                // Complete the turn to clear active_cell.
                self.chat.complete_turn();
                self.turn_running = false;
                // TODO: display error in chat (e.g. as a styled AgentCell)
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
                // Scrollback insertion is handled in `run_tui` (which owns the
                // terminal). See `commit_history_to_scrollback` in lib.rs.
            }
            AppEvent::UpdateStatus(state) => {
                self.status_bar.set_state(state);
            }
            AppEvent::ShowOverlay(OverlayKind::Transcript) => {
                self.overlay = Some(TranscriptOverlay::from_cells(
                    self.chat.committed_cells(),
                    80, // will be updated on next resize; 80 is a reasonable default
                ));
                self.frame_requester.schedule_frame();
            }
            AppEvent::CloseOverlay => {
                self.overlay = None;
                self.frame_requester.schedule_frame();
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
        // On the welcome screen, any key dismisses it and transitions to chat.
        if matches!(self.screen, AppScreen::Welcome) {
            self.screen = AppScreen::Chat;
            self.frame_requester.schedule_frame();
            // Forward printable characters to the input widget so the user
            // doesn't lose the first character they type.
            if let KeyCode::Char(_) = key.code {
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT {
                    self.chat.input.handle_key(key);
                }
            }
            return;
        }

        // When an overlay is active, route all keys to it.
        if let Some(overlay) = &mut self.overlay {
            // visible rows = viewport height minus header row inside the overlay
            let visible_rows = self.viewport_height.saturating_sub(1).max(1);
            let action = overlay.handle_key(key, visible_rows);
            if matches!(action, TranscriptAction::Close) {
                self.overlay = None;
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // Ctrl+T — toggle transcript overlay.
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.overlay = Some(TranscriptOverlay::from_cells(
                self.chat.committed_cells(),
                80,
            ));
            self.frame_requester.schedule_frame();
            return;
        }

        // When the command popup is visible, route keys to it first.
        if self.command_popup.is_visible() {
            match self.command_popup.handle_key(key) {
                CommandAction::Select(cmd) => {
                    // Clear the input (which contained the typed slash command).
                    self.chat.input.clear();
                    let _ = self.app_tx.send(AppEvent::SlashCommand(cmd));
                }
                CommandAction::Dismiss => {
                    // Clear the slash prefix from the input as well.
                    self.chat.input.clear();
                }
                CommandAction::None => {}
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // For Ctrl+C and Ctrl+D we check the app-level concern first, then
        // also delegate to the input widget so it can handle its own
        // Ctrl+D (delete) / Ctrl+C (interrupt) behaviour.
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.turn_running {
                    let _ = self.submission_tx.send(Submission::Interrupt);
                } else {
                    // Pass to input widget — InputAction::Interrupt is a no-op
                    // when nothing is running.
                    let _ = self.chat.input.handle_key(key);
                }
            }
            _ => {
                let action = self.chat.input.handle_key(key);
                // After any key, check whether the input now starts with `/`
                // at position 0 (sole character or first char). If so, show
                // the popup and sync the query portion (everything after `/`).
                let input_text = self.chat.input.text().to_owned();
                if input_text.starts_with('/') {
                    let query = &input_text[1..]; // everything after '/'
                    if !self.command_popup.is_visible() {
                        self.command_popup.show();
                    }
                    self.command_popup.update_query(query);
                } else if self.command_popup.is_visible() {
                    self.command_popup.hide();
                }
                match action {
                    InputAction::Submit(text) => self.submit_text(text),
                    InputAction::Exit => self.should_exit = true,
                    InputAction::Interrupt => {
                        if self.turn_running {
                            let _ = self.submission_tx.send(Submission::Interrupt);
                        }
                    }
                    InputAction::None => {}
                }
            }
        }
        self.frame_requester.schedule_frame();
    }

    /// Submit a user message: record it in the chat widget and send to agent.
    fn submit_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.chat.input.push_history(text.clone());
        self.chat.add_user_message(text.clone());
        let _ = self.submission_tx.send(Submission::UserMessage {
            text,
            images: vec![],
        });
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
        let welcome = WelcomeWidget::new(
            crate::widgets::welcome::WelcomeInfo {
                model: "test".to_string(),
                cwd: "/tmp".to_string(),
                version: "0.0.0".to_string(),
            },
            &[],
            &[],
        );
        let app = App {
            submission_tx,
            app_tx,
            frame_requester,
            turn_running: false,
            should_exit: false,
            screen: AppScreen::Chat, // Start in Chat for existing tests
            welcome,
            chat: ChatWidget::new(),
            status_bar: StatusBarWidget::new("test".to_string()),
            overlay: None,
            viewport_height: 24,
            command_popup: CommandPopup::new(),
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

    #[tokio::test]
    async fn enter_submits_message_to_agent() {
        let (mut app, mut sub_rx, _app_rx) = make_app();
        // Type "hello" then press Enter.
        for c in "hello".chars() {
            app.handle_tui_event(TuiEvent::Key(KeyEvent::new(
                KeyCode::Char(c),
                KeyModifiers::NONE,
            )));
        }
        app.handle_tui_event(TuiEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        match sub_rx.try_recv() {
            Ok(Submission::UserMessage { text, .. }) => {
                assert_eq!(text, "hello");
            }
            other => panic!("expected UserMessage, got {:?}", other),
        }
        // Input should be cleared.
        assert_eq!(app.chat.input.text(), "");
        // User message committed to chat.
        assert_eq!(app.chat.committed_cells().len(), 1);
    }

    #[tokio::test]
    async fn paste_inserts_into_input() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.handle_tui_event(TuiEvent::Paste("pasted text".into()));
        assert_eq!(app.chat.input.text(), "pasted text");
    }

    #[tokio::test]
    async fn text_delta_appends_to_active_cell() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.handle_agent_event(AgentEvent::TurnStarted);
        app.handle_agent_event(AgentEvent::TextDelta("Hello".into()));
        app.handle_agent_event(AgentEvent::TextDelta(" world".into()));
        let active = app.chat.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "Hello world");
    }

    #[tokio::test]
    async fn turn_complete_freezes_active_cell() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.handle_agent_event(AgentEvent::TurnStarted);
        app.handle_agent_event(AgentEvent::TextDelta("Response text".into()));
        app.handle_agent_event(AgentEvent::TurnComplete {
            response: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            turns_used: 1,
            tool_calls_made: 0,
        });
        assert!(app.chat.active_cell.is_none());
        assert!(!app.chat.committed_cells().is_empty());
    }

    #[tokio::test]
    async fn welcome_screen_transitions_to_chat_on_enter() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.screen = AppScreen::Welcome;
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_tui_event(TuiEvent::Key(key));
        assert_eq!(app.screen, AppScreen::Chat);
        // Enter should not leave text in the input buffer.
        assert_eq!(app.chat.input.text(), "");
    }

    #[tokio::test]
    async fn welcome_screen_forwards_printable_char() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.screen = AppScreen::Welcome;
        let key = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE);
        app.handle_tui_event(TuiEvent::Key(key));
        assert_eq!(app.screen, AppScreen::Chat);
        // The 'h' should have been forwarded to the input widget.
        assert_eq!(app.chat.input.text(), "h");
    }

    #[tokio::test]
    async fn welcome_screen_transitions_on_escape() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        app.screen = AppScreen::Welcome;
        let key = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        app.handle_tui_event(TuiEvent::Key(key));
        assert_eq!(app.screen, AppScreen::Chat);
        assert_eq!(app.chat.input.text(), "");
    }
}

//! Application state and event dispatch.

use crate::events::{
    AgentEvent, AgentMode, AppEvent, OverlayKind, StatusState, Submission, TuiEvent,
};
use crate::frame_requester::FrameRequester;
use crate::widgets::braille_canvas::Pattern;
use crate::widgets::chat_widget::ChatWidget;
use crate::widgets::clarification::{ClarificationAction, ClarificationWidget};
use crate::widgets::command_popup::{CommandAction, CommandPopup};
use crate::widgets::file_completion::{FileCompletion, FileCompletionAction};
use crate::widgets::help_overlay::{HelpAction, HelpOverlay};
use crate::widgets::input_widget::InputAction;
use crate::widgets::model_picker::{ModelPicker, ModelPickerAction};
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

/// Active fullscreen overlay.
pub enum ActiveOverlay {
    /// Scrollable conversation transcript.
    Transcript(TranscriptOverlay),
    /// Keybinding and command reference.
    Help(HelpOverlay),
    /// Interactive model picker.
    Models(ModelPicker),
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
    /// When the current turn started (for elapsed time display).
    pub turn_start: Option<std::time::Instant>,
    /// Which screen is currently active.
    pub screen: AppScreen,
    /// The welcome screen widget.
    pub welcome: WelcomeWidget,
    /// Composed chat area: history cells + active streaming cell + input.
    pub chat: ChatWidget,
    /// Single-row status bar rendered at the bottom of the viewport.
    pub status_bar: StatusBarWidget,
    /// Active fullscreen overlay, if any.
    pub overlay: Option<ActiveOverlay>,
    /// Last known viewport height (used to pass visible_rows to the overlay).
    pub viewport_height: u16,
    /// Slash command popup (shown when input starts with `/`).
    pub command_popup: CommandPopup,
    /// Interactive clarification picker (shown when agent asks a question with choices).
    pub clarification: ClarificationWidget,
    /// Force a one-shot terminal clear on the next frame after leaving welcome.
    pub clear_after_welcome: bool,
    /// Approximate character count during current streaming response (for token estimate).
    pub streaming_chars: usize,
    /// Model context window size in tokens (for context % calculation).
    pub context_window_size: usize,
    /// Last known viewport width (used for transcript overlay).
    pub viewport_width: u16,
    /// Active tool approval overlay (shown when a tool needs user approval).
    pub approval: Option<crate::widgets::approval_overlay::ApprovalOverlay>,
    /// Channel to send approval responses back to the tool execution thread.
    pub approval_response: Option<std::sync::mpsc::Sender<bool>>,
    /// Queue of pending approval requests (processed one at a time).
    pub approval_queue: std::collections::VecDeque<crate::approval::ApprovalRequest>,
    /// Tools that the user chose to always approve for this session.
    pub always_approved_tools: std::collections::HashSet<String>,
    /// Post-render visual effects (tachyonfx).
    pub effects: crate::effects::GenesisEffects,
    /// Lissajous pattern shown below messages when idle.
    pub idle_pattern: Pattern,
    /// @-file completion popup.
    pub file_completion: FileCompletion,
    /// Current agent operating mode (Act / Plan).
    pub agent_mode: AgentMode,
    /// Active color theme for the TUI.
    pub active_theme: Box<dyn crate::theme::Theme>,
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
            TuiEvent::Resize { width, height } => {
                self.viewport_width = width;
                self.viewport_height = height;
                self.frame_requester.schedule_frame();
            }
            TuiEvent::Draw | TuiEvent::FocusGained | TuiEvent::FocusLost => {}
        }
    }

    /// Process an event from the agent streaming callback.
    ///
    /// Updates internal state and schedules a frame redraw. For batch
    /// processing of multiple queued events, use [`handle_agent_event_no_frame`]
    /// followed by a single `frame_requester.schedule_frame()` call.
    pub fn handle_agent_event(&mut self, event: AgentEvent) {
        self.handle_agent_event_no_frame(event);
        self.frame_requester.schedule_frame();
    }

    /// Process an agent event without scheduling a frame redraw.
    ///
    /// Use this in batch-drain loops where many events are processed in one
    /// `select!` iteration, followed by a single `schedule_frame()` call.
    pub fn handle_agent_event_no_frame(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted => {
                self.turn_running = true;
                self.turn_start = Some(std::time::Instant::now());
                self.streaming_chars = 0;
                self.chat.start_turn();
                let _ = self
                    .app_tx
                    .send(AppEvent::UpdateStatus(StatusState::Thinking));
            }
            AgentEvent::TextDelta(text) => {
                self.streaming_chars += text.len();
                self.chat.append_text(&text);
                // Transition to Streaming state with approximate token count.
                // Rough estimate: ~4 chars per token.
                let approx_tokens = (self.streaming_chars / 4) as u64;
                let _ = self
                    .app_tx
                    .send(AppEvent::UpdateStatus(StatusState::Streaming {
                        tokens: approx_tokens,
                    }));
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
                    .send(AppEvent::UpdateStatus(StatusState::ToolRunning {
                        tool_name,
                    }));
            }
            AgentEvent::ToolCallEnd {
                call_id,
                success,
                duration,
            } => {
                self.chat.tool_call_end(&call_id, success, duration);
                if !success {
                    // Flash a brief glitch effect over the message area to
                    // signal the tool failure visually.
                    let chat_area = ratatui::layout::Rect {
                        x: 0,
                        y: 0,
                        width: self.viewport_width,
                        height: self.viewport_height.saturating_sub(1),
                    };
                    self.effects.flash_error(chat_area);
                }
            }
            AgentEvent::TurnComplete {
                input_tokens,
                output_tokens,
                ..
            } => {
                self.chat.complete_turn();
                self.turn_running = false;
                self.turn_start = None;
                self.status_bar.tokens_in += input_tokens;
                self.status_bar.tokens_out += output_tokens;
                self.status_bar.turn_elapsed = None;
                self.status_bar
                    .record_turn_tokens(input_tokens, output_tokens);

                // Update context usage percentage. Use the latest turn's
                // input_tokens (not the cumulative sum) because each LLM call
                // re-sends the full conversation, so input_tokens already
                // represents the current context window consumption.
                if self.context_window_size > 0 {
                    let pct = ((input_tokens as u128 * 100) / self.context_window_size as u128)
                        .min(100) as u8;
                    self.status_bar.set_context_percent(pct);
                }

                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
                let _ = self.app_tx.send(AppEvent::CommitHistory);
            }
            AgentEvent::ClarificationNeeded(question) => {
                // Show interactive picker if the question has choices,
                // otherwise show it as text and unlock input.
                self.clarification.show(&question);
                // Complete the turn visually so the user can interact.
                self.chat.complete_turn();
                self.turn_running = false;
                self.turn_start = None;
                self.status_bar.turn_elapsed = None;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
                let _ = self.app_tx.send(AppEvent::CommitHistory);
            }
            AgentEvent::Error(_err) => {
                self.chat.complete_turn();
                self.turn_running = false;
                self.turn_start = None;
                self.status_bar.turn_elapsed = None;
                let _ = self.app_tx.send(AppEvent::UpdateStatus(StatusState::Idle));
            }
            AgentEvent::Warning(_) => {}
        }
    }

    /// Process an internal application event.
    pub fn handle_app_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::CommitHistory => {
                // Alternate-screen TUI owns all history rendering in overlays;
                // drain any terminal-scrollback queue so buffers don't grow.
                let _ = self.chat.drain_pending_scrollback();
            }
            AppEvent::UpdateStatus(state) => {
                self.status_bar.set_state(state);

                // Trigger transition effects if the state actually changed.
                if let Some((from, to)) = self.status_bar.last_state_change() {
                    if from != to {
                        // Compute status bar area from viewport dimensions.
                        let status_area = ratatui::layout::Rect {
                            x: 0,
                            y: self.viewport_height.saturating_sub(1),
                            width: self.viewport_width,
                            height: 1,
                        };
                        self.effects.on_status_change(from, to, status_area);
                    }
                }
            }
            AppEvent::ShowOverlay(OverlayKind::Transcript) => {
                self.command_popup.hide();
                let visible_rows = self.viewport_height.saturating_sub(2).max(1);
                let width = if self.viewport_width > 0 {
                    self.viewport_width
                } else {
                    80
                };
                self.overlay = Some(ActiveOverlay::Transcript(TranscriptOverlay::from_cells(
                    self.chat.committed_cells(),
                    width,
                    visible_rows,
                )));
                self.frame_requester.schedule_frame();
            }
            AppEvent::ShowOverlay(OverlayKind::Help) => {
                self.command_popup.hide();
                self.overlay = Some(ActiveOverlay::Help(HelpOverlay::new()));
                self.frame_requester.schedule_frame();
            }
            AppEvent::CloseOverlay => {
                self.overlay = None;
                self.frame_requester.schedule_frame();
            }
            AppEvent::SlashCommand(cmd) => match cmd.as_str() {
                "/exit" | "/quit" => self.should_exit = true,
                "/clear" => {
                    self.chat = ChatWidget::new();
                    self.frame_requester.schedule_frame();
                }
                "/help" => {
                    self.command_popup.hide();
                    self.overlay = Some(ActiveOverlay::Help(HelpOverlay::new()));
                    self.frame_requester.schedule_frame();
                }
                "/models" => {
                    self.command_popup.hide();
                    self.overlay = Some(ActiveOverlay::Models(ModelPicker::new_loading()));
                    self.frame_requester.schedule_frame();
                    let _ = self.app_tx.send(AppEvent::FetchModels);
                }
                "/plan" => {
                    self.agent_mode = self.agent_mode.toggle();
                    self.frame_requester.schedule_frame();
                }
                "/theme" => {
                    // Cycle through user-selectable themes (excludes nocolor which is env-activated).
                    let names = crate::theme::USER_THEME_NAMES;
                    let current = self.active_theme.name().to_owned();
                    let idx = names.iter().position(|&n| n == current).unwrap_or(0);
                    let next = names[(idx + 1) % names.len()];
                    self.active_theme = crate::theme::resolve_theme(next);
                    // Persist to config (best-effort, ignore errors).
                    let _ = genesis_config::set_theme(next);
                    self.frame_requester.schedule_frame();
                }
                "/compact" | "/summary" => {
                    self.chat
                        .set_tool_display(crate::history::tool_cell::ToolDisplayMode::Summary);
                    self.frame_requester.schedule_frame();
                }
                "/verbose" => {
                    self.chat
                        .set_tool_display(crate::history::tool_cell::ToolDisplayMode::Verbose);
                    self.frame_requester.schedule_frame();
                }
                "/grouped" => {
                    self.chat
                        .set_tool_display(crate::history::tool_cell::ToolDisplayMode::Grouped);
                    self.frame_requester.schedule_frame();
                }
                "/transcript" => {
                    self.command_popup.hide();
                    self.overlay = Some(ActiveOverlay::Transcript(TranscriptOverlay::from_cells(
                        self.chat.committed_cells(),
                        self.viewport_width,
                        24, // default visible rows
                    )));
                    self.frame_requester.schedule_frame();
                }
                _ => {}
            },
            AppEvent::ModelChanged(model) => {
                self.status_bar.set_model(model);
                self.frame_requester.schedule_frame();
            }
            AppEvent::FetchModels => {
                // Handled in lib.rs event loop — spawn async fetch.
            }
            AppEvent::ModelsFetched(result) => {
                // Deliver fetched models to the picker overlay.
                if let Some(ActiveOverlay::Models(ref mut picker)) = self.overlay {
                    match result {
                        Ok(models) => picker.set_models(models),
                        Err(err) => picker.set_error(err),
                    }
                }
                self.frame_requester.schedule_frame();
            }
        }
    }

    /// Route a single key event to the appropriate handler.
    fn handle_key(&mut self, key: KeyEvent) {
        // On the welcome screen, any key dismisses it and transitions to chat.
        if matches!(self.screen, AppScreen::Welcome) {
            self.effects.cancel_all();
            self.screen = AppScreen::Chat;
            self.clear_after_welcome = true;
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

        // When a tool approval is pending, route keys to it first.
        if let Some(approval) = &mut self.approval {
            use crate::widgets::approval_overlay::ApprovalAction;
            let action = approval.handle_key(key);
            match action {
                ApprovalAction::Approve | ApprovalAction::Deny | ApprovalAction::AlwaysApprove => {
                    let approved = matches!(
                        action,
                        ApprovalAction::Approve | ApprovalAction::AlwaysApprove
                    );
                    if matches!(action, ApprovalAction::AlwaysApprove) {
                        self.always_approved_tools
                            .insert(approval.tool_name().to_owned());
                    }
                    if let Some(tx) = self.approval_response.take() {
                        let _ = tx.send(approved);
                    }
                    self.approval = None;
                    // Process next queued approval request, if any.
                    self.pop_next_approval();
                }
                ApprovalAction::None => {}
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // When an overlay is active, route all keys to it.
        if let Some(overlay) = &mut self.overlay {
            let visible_rows = self.viewport_height.saturating_sub(2).max(1);
            let (should_close, model_selected) = match overlay {
                ActiveOverlay::Transcript(t) => (
                    matches!(t.handle_key(key, visible_rows), TranscriptAction::Close),
                    None,
                ),
                ActiveOverlay::Help(h) => (
                    matches!(h.handle_key(key, visible_rows), HelpAction::Close),
                    None,
                ),
                ActiveOverlay::Models(m) => match m.handle_key(key, visible_rows) {
                    ModelPickerAction::Close => (true, None),
                    ModelPickerAction::Select(id) => (true, Some(id)),
                    ModelPickerAction::None => (false, None),
                },
            };
            if should_close {
                self.overlay = None;
            }
            if let Some(model_id) = model_selected {
                // Update status bar display.
                let _ = self.app_tx.send(AppEvent::ModelChanged(model_id.clone()));
                // Switch the actual execution service model via submission channel.
                let _ = self.submission_tx.send(Submission::ModelSwitch(model_id));
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // When the clarification picker is visible, route keys to it first.
        if self.clarification.is_visible() {
            match self.clarification.handle_key(key) {
                ClarificationAction::Confirm(answer) => {
                    // Submit the chosen answer as a user message.
                    self.submit_text(answer);
                }
                ClarificationAction::Dismiss => {
                    // User cancelled — just close the picker.
                }
                ClarificationAction::None => {}
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // Ctrl+T — toggle transcript overlay.
        if key.code == KeyCode::Char('t') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.command_popup.hide();
            let visible_rows = self.viewport_height.saturating_sub(2).max(1);
            let width = if self.viewport_width > 0 {
                self.viewport_width
            } else {
                80
            };
            self.overlay = Some(ActiveOverlay::Transcript(TranscriptOverlay::from_cells(
                self.chat.committed_cells(),
                width,
                visible_rows,
            )));
            self.frame_requester.schedule_frame();
            return;
        }

        // Ctrl+P — open command palette (accessible anytime).
        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if !self.command_popup.is_visible() {
                self.command_popup.show();
            } else {
                self.command_popup.hide();
            }
            self.frame_requester.schedule_frame();
            return;
        }

        // When the command popup is visible, route keys to it first.
        if self.command_popup.is_visible() {
            // Keep the slash command query synced with the actual input widget so
            // the popup always reflects what the user typed.
            let is_text_key = matches!(key.code, KeyCode::Char(_))
                && (key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT);
            if is_text_key || matches!(key.code, KeyCode::Backspace) {
                self.chat.input.handle_key(key);
            }

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

            // Keep popup query state aligned with the actual input buffer.
            let input_text = self.chat.input.text().to_owned();
            if let Some(query) = input_text.strip_prefix('/') {
                if !self.command_popup.is_visible() {
                    self.command_popup.show();
                }
                self.command_popup.update_query(query);
            } else if self.command_popup.is_visible() {
                self.command_popup.hide();
            }

            self.frame_requester.schedule_frame();
            return;
        }

        // When the @-file completion popup is visible, route keys to it.
        if self.file_completion.is_visible() {
            let is_text_key = matches!(key.code, KeyCode::Char(_))
                && (key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT);
            if is_text_key || matches!(key.code, KeyCode::Backspace) {
                self.chat.input.handle_key(key);
            }

            match self.file_completion.handle_key(key) {
                FileCompletionAction::Select(path) => {
                    // Replace @query with the selected path, preserving
                    // any text before and after the @-query token.
                    let text = self.chat.input.text().to_owned();
                    if let Some(at_pos) = text.rfind('@') {
                        // Find end of the @-query: next space or end of text.
                        let query_end = text[at_pos + 1..]
                            .find(' ')
                            .map(|p| at_pos + 1 + p)
                            .unwrap_or(text.len());
                        let before = &text[..at_pos];
                        let after = &text[query_end..];
                        self.chat.input.clear();
                        let new_text = format!("{before}{path} {}", after.trim_start());
                        for c in new_text.trim_end().chars() {
                            self.chat
                                .input
                                .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
                        }
                        // Add trailing space.
                        self.chat
                            .input
                            .handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
                    }
                }
                FileCompletionAction::Dismiss => {}
                FileCompletionAction::None => {}
            }

            // Update file completion query from input.
            let input_text = self.chat.input.text().to_owned();
            if let Some(at_pos) = input_text.rfind('@') {
                let query = &input_text[at_pos + 1..];
                self.file_completion.update_query(query);
            } else {
                self.file_completion.hide();
            }

            self.frame_requester.schedule_frame();
            return;
        }

        // Tab toggles Plan/Act mode when no overlay/popup is active
        // and the input field is empty (avoids accidental mode switch mid-typing).
        if key.code == KeyCode::Tab
            && key.modifiers == KeyModifiers::NONE
            && self.chat.input.text().is_empty()
        {
            self.agent_mode = self.agent_mode.toggle();
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
                if let Some(query) = input_text.strip_prefix('/') {
                    if !self.command_popup.is_visible() {
                        self.command_popup.show();
                    }
                    self.command_popup.update_query(query);
                } else if self.command_popup.is_visible() {
                    self.command_popup.hide();
                }

                // Check for @ at the end of input to trigger file completion.
                // Only triggers when @ was just typed (last char), not when the
                // buffer just happens to contain @ from earlier.
                if input_text.ends_with('@') && !self.file_completion.is_visible() {
                    self.file_completion.show();
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

    /// Check whether the approval overlay has timed out, and auto-deny if so.
    /// Returns `true` if the approval was expired and denied.
    pub fn check_approval_timeout(&mut self) -> bool {
        let expired = self.approval.as_ref().is_some_and(|a| a.is_expired());
        if expired {
            if let Some(tx) = self.approval_response.take() {
                let _ = tx.send(false);
            }
            self.approval = None;
            self.pop_next_approval();
            return true;
        }
        false
    }

    /// Pop the next queued approval request and show it, if any.
    /// Auto-approves queued requests for always-approved tools.
    fn pop_next_approval(&mut self) {
        while let Some(req) = self.approval_queue.pop_front() {
            if self.always_approved_tools.contains(&req.tool_name) {
                // Auto-approve without showing the overlay.
                let _ = req.response_tx.send(true);
                continue;
            }
            self.approval = Some(crate::widgets::approval_overlay::ApprovalOverlay::new(
                req.tool_name,
                &req.arguments,
            ));
            self.approval_response = Some(req.response_tx);
            return;
        }
    }

    /// Submit a user message: record it in the chat widget and send to agent.
    fn submit_text(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.chat.input.push_history(text.clone());
        self.chat.add_user_message(text.clone());

        // In Plan mode, prepend a system instruction so the LLM restricts
        // itself to read-only analysis and planning.
        let prompt = if self.agent_mode.is_plan() {
            format!(
                "[PLAN MODE - Read and analyze only. Do NOT edit files, run commands, or make \
                 changes. Discuss your plan and ask for confirmation before switching to Act mode.]\n\n{}",
                text
            )
        } else {
            text
        };

        let _ = self.submission_tx.send(Submission::UserMessage {
            text: prompt,
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
                backend: "test-backend".to_string(),
                session_id: "session-test".to_string(),
                cwd: "/tmp".to_string(),
                version: "0.0.0".to_string(),
                tool_count_builtin: 0,
                tool_count_mcp: 0,
                skill_count: 0,
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
            turn_start: None,
            screen: AppScreen::Chat, // Start in Chat for existing tests
            welcome,
            chat: ChatWidget::new(),
            status_bar: StatusBarWidget::new("test".to_string()),
            overlay: None,
            viewport_height: 24,
            command_popup: CommandPopup::new(),
            clarification: ClarificationWidget::new(),
            clear_after_welcome: false,
            streaming_chars: 0,
            context_window_size: 128_000,
            viewport_width: 80,
            approval: None,
            approval_response: None,
            approval_queue: std::collections::VecDeque::new(),
            always_approved_tools: std::collections::HashSet::new(),
            effects: crate::effects::GenesisEffects::new(false, false, Default::default()),
            idle_pattern: Pattern::Lissajous {
                t: 0.0,
                a: 3.0,
                b: 2.0,
                delta: std::f64::consts::FRAC_PI_2,
            },
            file_completion: FileCompletion::new(),
            agent_mode: AgentMode::default(),
            active_theme: crate::theme::theme_by_name("eve"),
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

    #[tokio::test]
    async fn tab_toggles_agent_mode() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        assert_eq!(app.agent_mode, AgentMode::Act);
        app.handle_tui_event(TuiEvent::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.agent_mode, AgentMode::Plan);
        app.handle_tui_event(TuiEvent::Key(KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )));
        assert_eq!(app.agent_mode, AgentMode::Act);
    }

    #[tokio::test]
    async fn slash_plan_toggles_agent_mode() {
        let (mut app, _sub_rx, _app_rx) = make_app();
        assert_eq!(app.agent_mode, AgentMode::Act);
        app.handle_app_event(AppEvent::SlashCommand("/plan".into()));
        assert_eq!(app.agent_mode, AgentMode::Plan);
        app.handle_app_event(AppEvent::SlashCommand("/plan".into()));
        assert_eq!(app.agent_mode, AgentMode::Act);
    }

    #[tokio::test]
    async fn plan_mode_prepends_instruction_to_submission() {
        let (mut app, mut sub_rx, _app_rx) = make_app();
        // Switch to plan mode.
        app.agent_mode = AgentMode::Plan;
        // Type "analyze this" and submit.
        for c in "analyze this".chars() {
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
                assert!(
                    text.starts_with("[PLAN MODE"),
                    "plan mode should prepend instruction, got: {text}"
                );
                assert!(
                    text.contains("analyze this"),
                    "original message should be preserved"
                );
            }
            other => panic!("expected UserMessage, got {:?}", other),
        }
    }
}

//! Genesis TUI — ratatui-based terminal interface for Eve.

use std::future::Future;
use std::pin::Pin;

use crate::app::{App, AppScreen};
use crate::events::{AgentEvent, AppEvent, Submission, TuiEvent};
use crate::frame_requester::FrameRequester;

use crossterm::event::{Event as CrosstermEvent, EventStream};
use futures_util::StreamExt;
use genesis_config::GenesisConfig;
use genesis_storage::SkillStore;
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{
    SessionExecutionError, SessionExecutionService, SessionTurnInput, SessionTurnOutcome,
};
use genesis_types::DeliveryPlatform;
use ratatui::{
    layout::Rect,
    style::{Color, Style},
};
use tokio::sync::{broadcast, mpsc};

pub mod app;
pub mod custom_terminal;
pub mod events;
pub mod frame_requester;
pub mod history;
pub mod render;
pub mod terminal;
pub mod widgets;

type TurnResult = Result<SessionTurnOutcome, SessionExecutionError>;

/// Return the recommended log file path for TUI mode: `~/.genesis/logs/tui.log`.
///
/// The caller (typically `main.rs` or the CLI `chat` command) should redirect
/// the global `tracing` subscriber to this file **before** calling [`run_tui`]
/// so that log output does not interfere with the ratatui viewport.
///
/// Returns `None` if the home directory cannot be determined.
pub fn tui_log_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".genesis/logs/tui.log"))
}

/// Build a pinned future that runs a single agent turn.
///
/// The key trick: `text` is moved *into* the returned async block, so the
/// `&str` borrow inside `SessionTurnInput` points at data owned by the
/// future itself. This avoids any cross-variable borrow issues in the
/// caller's `select!` loop.
fn make_turn_future<'a>(
    service: &'a SessionExecutionService<'a>,
    session_id: &'a str,
    text: String,
    agent_tx: mpsc::UnboundedSender<AgentEvent>,
) -> Pin<Box<dyn Future<Output = TurnResult> + 'a>> {
    Box::pin(async move {
        let input = SessionTurnInput {
            session_id,
            session_platform: "cli",
            delivery_platform: DeliveryPlatform::Cli,
            prompt: &text,
            title: None,
            images: vec![],
        };

        service
            .run_turn_streaming(input, move |event| match event {
                StreamEvent::Chunk(c) => {
                    let _ = agent_tx.send(AgentEvent::TextDelta(c.to_string()));
                }
                StreamEvent::ToolCallStart {
                    name,
                    call_id,
                    args_summary,
                } => {
                    let _ = agent_tx.send(AgentEvent::ToolCallStart {
                        call_id: call_id.to_string(),
                        tool_name: name.to_string(),
                        args_summary,
                    });
                }
                StreamEvent::ToolCallEnd {
                    call_id,
                    success,
                    duration_ms,
                    ..
                } => {
                    let _ = agent_tx.send(AgentEvent::ToolCallEnd {
                        call_id: call_id.to_string(),
                        success,
                        duration: std::time::Duration::from_millis(duration_ms),
                    });
                }
                StreamEvent::ClarificationNeeded { question } => {
                    let _ =
                        agent_tx.send(AgentEvent::ClarificationNeeded(question.to_string()));
                }
                StreamEvent::TurnStarted
                | StreamEvent::TokenUsage { .. }
                | StreamEvent::Warning(_) => {}
            })
            .await
    })
}

/// Entry point for the ratatui TUI.
///
/// Called from `genesis chat --tui` (the default).
///
/// ## Lifetime design
///
/// `SessionExecutionService<'a>` borrows `&'a LoadedConfig`, so we cannot
/// `tokio::spawn` the turn future (it would require `'static`). Instead we
/// keep it as `Option<Pin<Box<dyn Future + '_>>>` and poll it inside
/// `tokio::select!`. The `'_` ties the future to `service`'s lifetime,
/// which lives for the entire function call.
///
/// The user's prompt text is moved into the future via [`make_turn_future`],
/// so there is no separate `pending_text` variable that would create
/// cross-borrow issues in the `select!` macro expansion.
pub async fn run_tui(
    config: &GenesisConfig,
    service: &SessionExecutionService<'_>,
    session_id: &str,
) -> Result<(), TuiError> {
    terminal::init()?;

    let size = crossterm::terminal::size()?;
    let mut term = custom_terminal::CustomTerminal::new(size.0, size.1)?;
    let viewport_area = Rect::new(0, 0, size.0, size.1);
    term.set_viewport_area(viewport_area);

    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (submission_tx, mut submission_rx) = mpsc::unbounded_channel::<Submission>();
    let (draw_tx, mut draw_rx) = broadcast::channel::<()>(16);

    let frame_requester = FrameRequester::new(draw_tx);

    let (tool_count_builtin, tool_count_mcp) = service.tool_counts().await;
    let skill_count = SkillStore::new(&config.storage.database_path)
        .list_all()
        .map_or(0, |skills| skills.len());

    let full_art = genesis_ui::banner::full_art();
    let compact_art = genesis_ui::banner::compact_art();

    let welcome = crate::widgets::welcome::WelcomeWidget::new(
        crate::widgets::welcome::WelcomeInfo {
            model: config.provider.model.clone(),
            backend: config.provider.backend.clone(),
            session_id: session_id.to_string(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            tool_count_builtin,
            tool_count_mcp,
            skill_count,
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        &full_art,
        &compact_art,
    );

    let mut app = App {
        submission_tx,
        app_tx,
        frame_requester,
        turn_running: false,
        should_exit: false,
        turn_start: None,
        screen: AppScreen::Welcome,
        welcome,
        chat: crate::widgets::chat_widget::ChatWidget::new(),
        status_bar: crate::widgets::status_bar::StatusBarWidget::new(
            "default".to_string(), // TODO: get from config
        ),
        overlay: None,
        viewport_height: viewport_area.height,
        command_popup: crate::widgets::command_popup::CommandPopup::new(),
        clarification: crate::widgets::clarification::ClarificationWidget::new(),
        clear_after_welcome: false,
    };

    // Schedule an initial frame so the UI renders immediately.
    app.frame_requester.schedule_frame();

    let mut crossterm_events = EventStream::new();

    // The turn future borrows `service` (lifetime 'a) so it can't be spawned.
    // It lives here as an Option and gets polled in select!.
    let mut turn_future: Option<Pin<Box<dyn Future<Output = TurnResult> + '_>>> = None;

    loop {
        tokio::select! {
            // ── Terminal events — always active ──────────────────────
            ct_event = crossterm_events.next() => {
                if let Some(Ok(event)) = ct_event {
                    if let Some(tui_event) = translate_crossterm(event) {
                        // Intercept Resize to update the terminal viewport
                        // before delegating to App (which schedules a frame).
                        if let TuiEvent::Resize { width, height } = &tui_event {
                            let clamped = Rect::new(0, 0, *width, *height);
                            term.set_viewport_area(clamped);
                            app.viewport_height = clamped.height;
                        }
                        app.handle_tui_event(tui_event);
                    }
                }
            }

            // ── Accept submissions ONLY when no turn is running ─────
            submission = submission_rx.recv(), if turn_future.is_none() => {
                match submission {
                    Some(Submission::UserMessage { text, .. }) => {
                        let tx = agent_tx.clone();
                        let _ = tx.send(AgentEvent::TurnStarted);

                        turn_future = Some(make_turn_future(
                            service,
                            session_id,
                            text,
                            tx,
                        ));
                    }
                    Some(Submission::Interrupt) => {
                        // Drop the turn future to cancel
                        turn_future = None;
                    }
                    Some(Submission::Compact) => {
                        // TODO: trigger context compression
                    }
                    None => break,
                }
            }

            // ── Poll the running turn future ────────────────────────
            result = async {
                match turn_future.as_mut() {
                    Some(fut) => fut.as_mut().await,
                    None => std::future::pending::<TurnResult>().await,
                }
            }, if turn_future.is_some() => {
                turn_future = None;
                match result {
                    Ok(outcome) => {
                        let _ = agent_tx.send(AgentEvent::TurnComplete {
                            response: outcome.result.response,
                            input_tokens: outcome.result.total_input_tokens,
                            output_tokens: outcome.result.total_output_tokens,
                            turns_used: outcome.result.turns_used,
                            tool_calls_made: outcome.result.tool_calls_made,
                        });
                    }
                    Err(e) => {
                        let _ = agent_tx.send(AgentEvent::Error(e.to_string()));
                    }
                }
            }

            // ── Agent events (from streaming callback via channel) ──
            agent_event = agent_rx.recv() => {
                if let Some(event) = agent_event {
                    app.handle_agent_event(event);
                }
            }

            // ── Internal app events ─────────────────────────────────
            app_event = app_rx.recv() => {
                if let Some(event) = app_event {
                    if matches!(&event, AppEvent::CommitHistory) {
                        // In alternate-screen mode, history insertion is handled
                        // by the transcript overlay and does not write to
                        // terminal scrollback.
                    }
                    app.handle_app_event(event);
                }
            }

            // ── Frame draw timer ────────────────────────────────────
            draw_result = draw_rx.recv() => {
                // Break on channel close to avoid an infinite render spin.
                if matches!(draw_result, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }

                // Advance status bar animation (sprite / spinner).
                app.status_bar.tick();

                // Update elapsed time for the current turn.
                if app.turn_running {
                    if let Some(start) = app.turn_start {
                        app.status_bar.turn_elapsed = Some(start.elapsed());
                    }
                }

                if app.clear_after_welcome {
                    let _ = term.clear_all();
                    app.clear_after_welcome = false;
                }

                render_frame(&mut term, &mut app);

                // Schedule periodic redraws while animations are active.
                if app.status_bar.is_animating() {
                    app.frame_requester
                        .schedule_frame_in(app.status_bar.animation_interval());
                }
            }
        }

        if app.should_exit {
            break;
        }
    }

    terminal::restore()?;
    Ok(())
}

/// Render one frame: draw the active screen into the terminal's buffer and flush.
///
/// When an overlay is active it occupies the full viewport (no status bar).
/// Otherwise, the chat widget and status bar share the viewport as usual.
fn render_frame(term: &mut custom_terminal::CustomTerminal, app: &mut App) {
    let area = term.viewport_area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let buf = term.current_buffer_mut();
    // Clear the buffer before drawing so stale content doesn't linger.
    buf.reset();

    // ── Overlay (Transcript) takes over the full viewport ─────────────────
    if let Some(overlay) = &app.overlay {
        overlay.render(area, buf);
        let _ = term.draw_diff();
        term.swap_buffers();
        let _ = term.flush();
        return;
    }

    match app.screen {
        AppScreen::Welcome => {
            // Welcome screen occupies the full viewport.
            app.welcome.render(area, buf);
        }
        AppScreen::Chat => {
            // Reserve the final row for status, leave space for a bounded input
            // panel and a separator between messages and input.
            if area.height < 2 {
                app.status_bar.render(area, buf);
            } else {
                const INPUT_PANEL_ROWS: u16 = 3;
                const SEPARATOR_ROW: u16 = 1;
                const STATUS_ROWS: u16 = 1;

                let status_area = Rect {
                    x: area.x,
                    y: area.y + area.height - STATUS_ROWS,
                    width: area.width,
                    height: STATUS_ROWS,
                };
                app.status_bar.render(status_area, buf);

                let chat_area_height = area.height - STATUS_ROWS;
                let input_rows = INPUT_PANEL_ROWS.min(chat_area_height);
                let message_area_height = if chat_area_height > input_rows {
                    chat_area_height.saturating_sub(input_rows + 1)
                } else {
                    0
                };
                let separator_rows = if message_area_height > 0 { SEPARATOR_ROW } else { 0 };

                let message_area = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: message_area_height,
                };
                let separator_area = Rect {
                    x: area.x,
                    y: area.y + message_area_height,
                    width: area.width,
                    height: separator_rows,
                };
                let input_area = Rect {
                    x: area.x,
                    y: area.y + message_area_height + separator_rows,
                    width: area.width,
                    height: input_rows,
                };
                let interactive_area = Rect {
                    x: area.x,
                    y: area.y,
                    width: area.width,
                    height: chat_area_height,
                };

                if message_area_height > 0 {
                    app.chat.render_messages(message_area, buf);
                }

                if separator_rows > 0 {
                    let line_style = Style::default().fg(Color::Rgb(72, 72, 72));
                    let sep_row = separator_area.y;
                    for x in separator_area.x..separator_area.x + separator_area.width {
                        if let Some(cell) = buf.cell_mut((x, sep_row)) {
                            cell.set_symbol("─");
                            cell.set_style(line_style);
                        }
                    }
                }

                if input_area.height >= 2 && input_area.width >= 2 {
                    // Input panel border.
                    let border_style = Style::default().fg(Color::Rgb(108, 108, 108));
                    let right = input_area.x + input_area.width - 1;
                    let bottom = input_area.y + input_area.height - 1;

                    if let Some(cell) = buf.cell_mut((input_area.x, input_area.y)) {
                        cell.set_symbol("┌");
                        cell.set_style(border_style);
                    }
                    for col in (input_area.x + 1)..right {
                        if let Some(cell) = buf.cell_mut((col, input_area.y)) {
                            cell.set_symbol("─");
                            cell.set_style(border_style);
                        }
                    }
                    if let Some(cell) = buf.cell_mut((right, input_area.y)) {
                        cell.set_symbol("┐");
                        cell.set_style(border_style);
                    }

                    for row in (input_area.y + 1)..bottom {
                        if let Some(cell) = buf.cell_mut((input_area.x, row)) {
                            cell.set_symbol("│");
                            cell.set_style(border_style);
                        }
                        if let Some(cell) = buf.cell_mut((right, row)) {
                            cell.set_symbol("│");
                            cell.set_style(border_style);
                        }
                    }

                    if let Some(cell) = buf.cell_mut((input_area.x, bottom)) {
                        cell.set_symbol("└");
                        cell.set_style(border_style);
                    }
                    for col in (input_area.x + 1)..right {
                        if let Some(cell) = buf.cell_mut((col, bottom)) {
                            cell.set_symbol("─");
                            cell.set_style(border_style);
                        }
                    }
                    if let Some(cell) = buf.cell_mut((right, bottom)) {
                        cell.set_symbol("┘");
                        cell.set_style(border_style);
                    }

                    let inner = Rect {
                        x: input_area.x + 1,
                        y: input_area.y + 1,
                        width: input_area.width - 2,
                        height: input_area.height - 2,
                    };
                    if inner.width > 0 && inner.height > 0 {
                        app.chat.render_input(inner, buf, app.turn_running);
                    }
                } else {
                    app.chat.render_input(input_area, buf, app.turn_running);
                }

                // Render the slash command popup above the input area.
                if app.command_popup.is_visible() {
                    app.command_popup.render(interactive_area, buf);
                }

                // Render the clarification picker as a centered overlay.
                if app.clarification.is_visible() {
                    app.clarification.render(area, buf);
                }
            }
        }
    }

    // Write only changed cells to the terminal, then swap buffers.
    let _ = term.draw_diff();
    term.swap_buffers();
    let _ = term.flush();
}

/// Convert crossterm events to TUI events.
pub fn translate_crossterm(event: CrosstermEvent) -> Option<TuiEvent> {
    match event {
        CrosstermEvent::Key(key) => Some(TuiEvent::Key(key)),
        CrosstermEvent::Paste(text) => Some(TuiEvent::Paste(text)),
        CrosstermEvent::Resize(w, h) => Some(TuiEvent::Resize {
            width: w,
            height: h,
        }),
        CrosstermEvent::FocusGained => Some(TuiEvent::FocusGained),
        CrosstermEvent::FocusLost => Some(TuiEvent::FocusLost),
        _ => None,
    }
}

/// Errors that can occur in the TUI.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    #[error("agent error: {0}")]
    Agent(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_key_event() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        let ct = CrosstermEvent::Key(key);
        match translate_crossterm(ct) {
            Some(TuiEvent::Key(k)) => assert_eq!(k.code, KeyCode::Char('a')),
            other => panic!("expected TuiEvent::Key, got {:?}", other),
        }
    }

    #[test]
    fn translate_paste_event() {
        let ct = CrosstermEvent::Paste("hello".into());
        match translate_crossterm(ct) {
            Some(TuiEvent::Paste(text)) => assert_eq!(text, "hello"),
            other => panic!("expected TuiEvent::Paste, got {:?}", other),
        }
    }

    #[test]
    fn translate_resize_event() {
        let ct = CrosstermEvent::Resize(80, 24);
        match translate_crossterm(ct) {
            Some(TuiEvent::Resize { width, height }) => {
                assert_eq!(width, 80);
                assert_eq!(height, 24);
            }
            other => panic!("expected TuiEvent::Resize, got {:?}", other),
        }
    }

    #[test]
    fn translate_focus_events() {
        assert!(matches!(
            translate_crossterm(CrosstermEvent::FocusGained),
            Some(TuiEvent::FocusGained)
        ));
        assert!(matches!(
            translate_crossterm(CrosstermEvent::FocusLost),
            Some(TuiEvent::FocusLost)
        ));
    }

    #[test]
    fn translate_mouse_returns_none() {
        let ct = CrosstermEvent::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(translate_crossterm(ct).is_none());
    }

    #[test]
    fn tui_log_path_returns_some() {
        let path = tui_log_path();
        assert!(path.is_some(), "tui_log_path() should return Some on a system with a home dir");
        let p = path.unwrap();
        assert!(p.ends_with("tui.log"));
        assert!(p.to_string_lossy().contains(".genesis/logs"));
    }
}

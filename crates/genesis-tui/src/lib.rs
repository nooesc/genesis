//! Genesis TUI — ratatui-based terminal interface for Eve.

use std::future::Future;
use std::pin::Pin;

use crate::app::{App, AppScreen};
use crate::events::{AgentEvent, AppEvent, Submission, TuiEvent};
use crate::frame_requester::FrameRequester;

use crossterm::event::{Event as CrosstermEvent, EventStream};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use futures_util::StreamExt;
use genesis_config::GenesisConfig;
use genesis_core::agent_loop::StreamEvent;
use genesis_core::execution::{
    SessionExecutionError, SessionExecutionService, SessionTurnInput, SessionTurnOutcome,
};
use genesis_storage::SkillStore;
use genesis_types::DeliveryPlatform;
use ratatui::{layout::Rect, style::Style};
use tokio::sync::{broadcast, mpsc};

pub mod app;
pub mod approval;
pub mod custom_terminal;
pub mod effects;
pub mod events;
pub mod frame_requester;
pub mod history;
pub mod render;
pub mod streaming;
pub mod terminal;
pub mod theme;
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
                    let _ = agent_tx.send(AgentEvent::ClarificationNeeded(question.to_string()));
                }
                StreamEvent::Warning(msg) => {
                    let _ = agent_tx.send(AgentEvent::Warning(msg.to_string()));
                }
                StreamEvent::TurnStarted | StreamEvent::TokenUsage { .. } => {}
            })
            .await
    })
}

// RAII guard that calls `terminal::restore()` on drop, ensuring the terminal
// is cleaned up even if `run_tui` returns early via `?`.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = terminal::restore();
    }
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
    service: &mut SessionExecutionService<'_>,
    session_id: &str,
    initial_prompt: Option<String>,
) -> Result<(), TuiError> {
    terminal::init()?;
    let _guard = TerminalGuard;

    let size = crossterm::terminal::size()?;
    let mut term = custom_terminal::CustomTerminal::new(size.0, size.1)?;
    let viewport_area = Rect::new(0, 0, size.0, size.1);
    term.set_viewport_area(viewport_area);

    let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (app_tx, mut app_rx) = mpsc::unbounded_channel::<AppEvent>();
    let (submission_tx, mut submission_rx) = mpsc::unbounded_channel::<Submission>();
    let (cancel_tx, mut cancel_rx) = mpsc::unbounded_channel::<()>();
    let (draw_tx, mut draw_rx) = broadcast::channel::<()>(16);

    // Set up TUI-based tool approval handler.
    let (approval_tx, mut approval_rx) =
        mpsc::unbounded_channel::<crate::approval::ApprovalRequest>();
    service.set_approval_handler(std::sync::Arc::new(
        crate::approval::TuiApprovalHandler::new(approval_tx),
    ));

    let frame_requester = FrameRequester::new(draw_tx);

    let (tool_count_builtin, tool_count_mcp) = service.tool_counts().await;
    let db_path = config.storage.database_path.clone();
    let skill_count = tokio::task::spawn_blocking(move || {
        SkillStore::new(&db_path)
            .list_all()
            .map_or(0, |skills| skills.len())
    })
    .await
    .unwrap_or(0);

    // Look up context window size for the configured model.
    // Strip provider prefix (e.g. "anthropic/claude-sonnet-4-6" → "claude-sonnet-4-6")
    // since model_metadata only indexes bare model IDs.
    let model_for_lookup = config
        .provider
        .model
        .rsplit('/')
        .next()
        .unwrap_or(&config.provider.model);
    let context_window_size = genesis_provider::model_metadata::lookup(model_for_lookup)
        .map(|m| m.context_length)
        .unwrap_or(genesis_provider::model_metadata::DEFAULT_CONTEXT_LENGTH);

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

    let animations_enabled = config.tui.animations
        && genesis_config::env::get_opt(genesis_config::env::REDUCE_MOTION)
            .is_none_or(|v| v != "1");
    let no_color = genesis_config::env::is_present(genesis_config::env::NO_COLOR);

    let right_info =
        tokio::task::spawn_blocking(crate::widgets::status_bar::StatusBarWidget::detect_right_info)
            .await
            .unwrap_or_else(|_| String::new());
    let mut status_bar =
        crate::widgets::status_bar::StatusBarWidget::new(config.provider.model.clone(), right_info);
    status_bar.set_effects_enabled(animations_enabled);

    let mut app = App {
        submission_tx,
        cancel_tx,
        app_tx,
        frame_requester,
        turn_running: false,
        should_exit: false,
        should_suspend: false,
        turn_start: None,
        screen: AppScreen::Welcome,
        welcome,
        chat: crate::widgets::chat_widget::ChatWidget::new(),
        status_bar,
        overlay: None,
        viewport_height: viewport_area.height,
        command_popup: crate::widgets::command_popup::CommandPopup::new(),
        clarification: crate::widgets::clarification::ClarificationWidget::new(),
        clear_after_welcome: false,
        streaming_chars: 0,
        context_window_size,
        viewport_width: viewport_area.width,
        approval: None,
        approval_response: None,
        approval_queue: std::collections::VecDeque::new(),
        always_approved_tools: std::collections::HashSet::new(),
        effects: crate::effects::GenesisEffects::new(
            animations_enabled,
            no_color,
            config.tui.effects.clone(),
        ),
        idle_pattern: crate::widgets::braille_canvas::Pattern::Lissajous {
            t: 0.0,
            a: 3.0,
            b: 2.0,
            delta: std::f64::consts::FRAC_PI_2,
        },
        file_completion: crate::widgets::file_completion::FileCompletion::new(),
        agent_mode: crate::events::AgentMode::default(),
        active_theme: crate::theme::resolve_theme(&config.tui.theme),
    };

    // Apply the initial theme to themed widgets.
    app.status_bar.set_theme(&*app.active_theme);
    app.chat.set_theme(&*app.active_theme);

    // If an initial prompt was provided, skip the welcome screen and submit
    // the prompt as the first user message once the event loop starts.
    let pending_initial_prompt = if initial_prompt.is_some() {
        app.screen = AppScreen::Chat;
        app.clear_after_welcome = true;
        initial_prompt
    } else {
        None
    };

    // Schedule an initial frame so the UI renders immediately.
    app.frame_requester.schedule_frame();

    let mut crossterm_events = EventStream::new();

    // The turn future borrows `service` (lifetime 'a) so it can't be spawned.
    // It lives here as an Option and gets polled in select!.
    let mut turn_future: Option<Pin<Box<dyn Future<Output = TurnResult> + '_>>> = None;

    let mut pending_model_switch: Option<String> = None;

    // Submit the initial prompt (from `genesis chat --tui "prompt"`) as the
    // first user message. This fires before the first select! iteration so it
    // will be picked up immediately by the submission_rx arm.
    if let Some(prompt) = pending_initial_prompt {
        app.chat.add_user_message(prompt.clone());
        let _ = app.submission_tx.send(Submission::UserMessage {
            text: prompt,
            images: vec![],
        });
    }

    loop {
        // Process deferred model switch — must drop turn_future first
        // to release the immutable borrow on `service`.
        if let Some(model_id) = pending_model_switch.take() {
            // Drop any active turn future to release the borrow on service.
            turn_future = None;
            let (backend, model) = if let Some(pos) = model_id.find('/') {
                (model_id[..pos].to_owned(), model_id.clone())
            } else {
                ("openrouter".to_owned(), model_id.clone())
            };
            service.set_model_override(backend, model);
            tracing::info!(model_id = model_id.as_str(), "model switched via picker");
        }

        // Use `biased;` so arms are polled in declaration order.
        // This ensures terminal events (resize, keys, Ctrl+C) are always
        // serviced first, even when the agent is streaming data rapidly.
        tokio::select! {
            biased;

            // ── 1. Terminal events (resize, keys) — highest priority ──
            ct_event = crossterm_events.next() => {
                if let Some(Ok(event)) = ct_event {
                    if let Some(tui_event) = translate_crossterm(event) {
                        // Intercept Resize to defer viewport update until
                        // the next frame draw. This coalesces rapid resize
                        // events (common during tmux drag-resize) into one
                        // screen clear + full redraw per frame interval.
                        if let TuiEvent::Resize { width, height } = &tui_event {
                            let clamped = Rect::new(0, 0, *width, *height);
                            term.set_viewport_area(clamped);
                            // Update App dimensions eagerly so layout queries
                            // between now and the frame draw use correct values.
                            app.viewport_width = clamped.width;
                            app.viewport_height = clamped.height;
                            // Schedule frame but skip App::handle_tui_event for
                            // Resize — the actual effects/scroll/overlay work
                            // happens when apply_pending_resize runs in the
                            // frame draw path.
                            app.frame_requester.schedule_frame();
                        } else {
                            app.handle_tui_event(tui_event);
                        }
                    }
                }
            }

            // ── 2. Cancel signal — must be responsive ───────────────
            _cancel = cancel_rx.recv() => {
                if turn_future.is_some() {
                    tracing::info!("cancelling running turn via Ctrl+C");
                    // Drop the turn future, which aborts the in-flight agent
                    // loop. The agent loop's async operations are cancelled
                    // cooperatively by the Tokio runtime when the future is
                    // dropped.
                    turn_future = None;
                    let _ = agent_tx.send(AgentEvent::Cancelled);
                }
            }

            // ── 3. Accept submissions ONLY when no turn is running ──
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
                        // Drop the turn future to cancel (fallback path —
                        // normally cancelled via the dedicated cancel channel).
                        turn_future = None;
                    }
                    Some(Submission::ModelSwitch(_model_id)) => {
                        // Model switch is handled below, outside the select
                        // block, to avoid borrow conflicts with turn_future.
                        // Store it for deferred processing.
                        pending_model_switch = Some(_model_id);
                    }
                    None => break,
                }
            }

            // ── 4. Poll the running turn future ─────────────────────
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
                            input_tokens: outcome.result.total_input_tokens as u64,
                            output_tokens: outcome.result.total_output_tokens as u64,
                            turns_used: outcome.result.turns_used,
                            tool_calls_made: outcome.result.tool_calls_made,
                        });
                    }
                    Err(e) => {
                        let _ = agent_tx.send(AgentEvent::Error(e.to_string()));
                    }
                }
            }

            // ── 5. Agent events (from streaming callback via channel) ──
            agent_event = agent_rx.recv() => {
                if let Some(event) = agent_event {
                    // Batch-process: drain all queued events before scheduling
                    // a single frame redraw. During streaming, many TextDelta
                    // events may arrive between frames; processing them all in
                    // one go avoids redundant select! iterations.
                    app.handle_agent_event_no_frame(event);
                    while let Ok(extra) = agent_rx.try_recv() {
                        app.handle_agent_event_no_frame(extra);
                    }
                    app.frame_requester.schedule_frame();
                }
            }

            // ── 6. Tool approval requests ───────────────────────────
            approval = approval_rx.recv() => {
                if let Some(req) = approval {
                    // Auto-approve if the user previously chose "always approve" for this tool.
                    if app.always_approved_tools.contains(&req.tool_name) {
                        let _ = req.response_tx.send(true);
                    } else if app.approval.is_some() {
                        // Already showing an approval — queue this one.
                        app.approval_queue.push_back(req);
                    } else {
                        // Show immediately.
                        app.approval = Some(
                            crate::widgets::approval_overlay::ApprovalOverlay::new(
                                req.tool_name,
                                &req.arguments,
                            ),
                        );
                        app.approval_response = Some(req.response_tx);
                    }
                    app.frame_requester.schedule_frame();
                }
            }

            // ── 7. Internal app events ──────────────────────────────
            app_event = app_rx.recv() => {
                if let Some(event) = app_event {
                    if matches!(&event, AppEvent::CommitHistory) {
                        // In alternate-screen mode, history insertion is handled
                        // by the transcript overlay and does not write to
                        // terminal scrollback.
                    }
                    if matches!(&event, AppEvent::FetchModels) {
                        // Spawn async model fetch.
                        let tx = app.app_tx.clone();
                        let cache_dir = config.storage.data_dir.join("cache");
                        tokio::spawn(async move {
                            let result =
                                genesis_provider::openrouter_models::fetch_models(None, &cache_dir)
                                    .await
                                    .map_err(|e| e.to_string());
                            let _ = tx.send(AppEvent::ModelsFetched(result));
                        });
                    }
                    app.handle_app_event(event);
                }
            }

            // ── 8. Frame draw timer — lowest priority ───────────────
            draw_result = draw_rx.recv() => {
                // Break on channel close to avoid an infinite render spin.
                if matches!(draw_result, Err(broadcast::error::RecvError::Closed)) {
                    break;
                }

                // Advance status bar animation (sprite / spinner + heartbeat).
                app.status_bar.tick();

                // Advance streaming animation: commit buffered text line-by-line.
                app.chat.tick_streaming();

                // Check if the approval overlay has timed out.
                app.check_approval_timeout();

                // Compute frame delta once — used by both braille patterns and tachyonfx.
                let frame_dt = app.effects.frame_dt();
                if animations_enabled {
                    if matches!(app.screen, AppScreen::Welcome) {
                        app.welcome.braille_pattern.tick(frame_dt);
                    }
                    if !app.turn_running {
                        app.idle_pattern.tick(frame_dt);
                    }
                }

                // Update elapsed time for the current turn.
                if app.turn_running {
                    if let Some(start) = app.turn_start {
                        app.status_bar.turn_elapsed = Some(start.elapsed());
                    }
                }

                if app.clear_after_welcome {
                    let _ = term.clear_all();
                    app.clear_after_welcome = false;
                    let area = term.viewport_area();
                    app.effects.start_chat_coalesce(area);
                    // Start idle effects on the status bar area.
                    let status_area = Rect {
                        x: area.x,
                        y: area.y + area.height.saturating_sub(1),
                        width: area.width,
                        height: 1,
                    };
                    app.effects.start_idle_effects(status_area);
                }

                // Sync agent mode to the status bar before rendering.
                app.status_bar.set_agent_mode(app.agent_mode);

                render_frame(&mut term, &mut app, frame_dt);

                // Schedule periodic redraws while animations are active.
                let has_tachyonfx = app.effects.is_running();
                let has_sprite_anim = app.status_bar.is_animating();
                let has_braille = animations_enabled
                    && (matches!(app.screen, AppScreen::Welcome)
                        || (!app.turn_running && matches!(app.screen, AppScreen::Chat)));
                let has_approval = app.approval.is_some();
                let has_streaming = app.chat.has_streaming_pending();
                if has_tachyonfx || has_sprite_anim || has_braille || has_approval || has_streaming {
                    let interval = if has_streaming {
                        // ~120fps for smooth streaming animation — matches Codex's
                        // TARGET_FRAME_INTERVAL for line-by-line commit ticks.
                        std::time::Duration::from_millis(8)
                    } else if has_tachyonfx {
                        std::time::Duration::from_millis(16) // ~60fps for tachyonfx effects
                    } else if has_sprite_anim {
                        app.status_bar.animation_interval()
                    } else if has_approval {
                        // Approval countdown updates once per second.
                        std::time::Duration::from_secs(1)
                    } else {
                        // Braille animations (matrix rain on welcome screen).
                        std::time::Duration::from_millis(50) // ~20fps
                    };
                    app.frame_requester.schedule_frame_in(interval);
                }
            }
        }

        // ── Handle Ctrl+Z suspension (unix only) ────────────────────
        // This runs outside the select! block so we can safely tear down
        // and re-initialize terminal state without interfering with async
        // polling.
        #[cfg(unix)]
        if app.should_suspend {
            app.should_suspend = false;

            // Skip suspend in terminal multiplexers — they handle Ctrl+Z
            // themselves and sending SIGTSTP can cause double-suspend or
            // unexpected detach behaviour.
            if !terminal::is_multiplexer() {
                let _ = terminal::restore();

                // SAFETY: Sending SIGTSTP to the process group (pid 0) is the
                // standard way to suspend a foreground process. The process will
                // stop here until the user types `fg` in the shell. After
                // resumption, execution continues at the next line.
                unsafe {
                    libc::kill(0, libc::SIGTSTP);
                }

                // Process resumes here after `fg`.
                terminal::init()?;

                // Re-query terminal size — it may have changed while suspended.
                let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
                let new_area = Rect::new(0, 0, w, h);

                // Always clear the terminal and force a full redraw after
                // resume. The alternate screen is freshly re-entered (blank),
                // so the diff renderer must emit all cells. Using clear_all
                // ensures this even when the size hasn't changed.
                let _ = term.clear_all();
                term.set_viewport_area(new_area);
                // If size changed, apply_pending_resize in render_frame will
                // handle it. If size is the same, clear_all already reset the
                // buffers for a full redraw.
                app.viewport_width = new_area.width;
                app.viewport_height = new_area.height;

                // Clear stale effect state from the pre-suspend viewport.
                app.effects.on_resize();

                // Force a full redraw so the screen is repainted cleanly.
                app.frame_requester.schedule_frame();
            }
        }

        if app.should_exit {
            break;
        }
    }

    // TerminalGuard handles terminal::restore() on drop, covering both
    // normal exit and early `?` returns.
    Ok(())
}

/// Render one frame: draw the active screen into the terminal's buffer and flush.
///
/// When an overlay is active it occupies the full viewport (no status bar).
/// Otherwise, the chat widget and status bar share the viewport as usual.
fn render_frame(
    term: &mut custom_terminal::CustomTerminal,
    app: &mut App,
    frame_dt: std::time::Duration,
) {
    // Apply any deferred resize before rendering. This coalesces rapid resize
    // events (tmux drag-resize sends many SIGWINCH in quick succession) into
    // a single screen clear + full redraw per frame interval.
    if term.apply_pending_resize() {
        let area = term.viewport_area();
        app.viewport_width = area.width;
        app.viewport_height = area.height;

        // Cancel all running effects — they hold stale coordinates from
        // the pre-resize viewport.
        app.effects.on_resize();

        // Reclamp scroll offset for the new width.
        app.chat.reclamp_scroll(area.width);

        // Rebuild the transcript overlay if open.
        if let Some(app::ActiveOverlay::Transcript(_)) = &app.overlay {
            let visible_rows = app.viewport_height.saturating_sub(1).max(1);
            app.overlay = Some(app::ActiveOverlay::Transcript(
                crate::widgets::transcript::TranscriptOverlay::from_cells(
                    app.chat.committed_cells(),
                    app.viewport_width,
                    visible_rows,
                ),
            ));
        }
    }

    let area = term.viewport_area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let buf = term.current_buffer_mut();
    // Clear the buffer before drawing so stale content doesn't linger.
    buf.reset();

    // ── Overlay takes over the full viewport ────────────────────────────
    if let Some(overlay) = &app.overlay {
        match overlay {
            app::ActiveOverlay::Transcript(t) => t.render(area, buf),
            app::ActiveOverlay::Help(h) => h.render(area, buf),
            app::ActiveOverlay::Models(m) => m.render(area, buf),
        }
        let _ = crossterm::execute!(term.backend_mut(), BeginSynchronizedUpdate);
        let _ = term.draw_diff();
        term.swap_buffers();
        let _ = crossterm::execute!(term.backend_mut(), EndSynchronizedUpdate);
        let _ = term.flush();
        return;
    }

    match app.screen {
        AppScreen::Welcome => {
            // Welcome screen occupies the full viewport.
            app.welcome.render(area, buf);

            // Trigger boot sequence on first welcome render.
            if !app.welcome.boot_triggered() {
                app.welcome.mark_boot_triggered();
                let areas = app.welcome.last_areas();
                app.effects.start_boot_sequence(areas.title, areas.status);
            }
        }
        AppScreen::Chat => {
            // Reserve the final row for status, leave space for a bounded input
            // panel and a separator between messages and input.
            if area.height < 2 {
                app.status_bar.render(area, buf);
            } else {
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

                // Dynamic input height: content lines + 2 border rows,
                // capped at 50% of available space.
                let content_rows = app.chat.input.height(area.width);
                let upper = (chat_area_height / 2).max(3).min(chat_area_height);
                let input_rows = if upper < 3 {
                    // Extremely short viewport — just use what we have.
                    upper
                } else {
                    (content_rows + 2).clamp(3, upper)
                };
                let message_area_height = if chat_area_height > input_rows {
                    chat_area_height.saturating_sub(input_rows + 1)
                } else {
                    0
                };
                let separator_rows = if message_area_height > 0 {
                    SEPARATOR_ROW
                } else {
                    0
                };

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

                    // Idle braille canvas: show a small Lissajous animation
                    // below the last message when the agent is idle and there
                    // is remaining vertical space (at least 5 rows).
                    const IDLE_CANVAS_HEIGHT: u16 = 5;
                    const IDLE_CANVAS_WIDTH: u16 = 15;
                    if !app.turn_running && app.effects.enabled() {
                        let content_h = app.chat.visible_content_height(area.width);
                        let remaining = message_area_height.saturating_sub(content_h);
                        if remaining >= IDLE_CANVAS_HEIGHT {
                            // Center the canvas horizontally in the message area.
                            let canvas_x = message_area.x
                                + message_area.width.saturating_sub(IDLE_CANVAS_WIDTH) / 2;
                            let canvas_y = message_area.y
                                + content_h
                                + (remaining.saturating_sub(IDLE_CANVAS_HEIGHT)) / 2;
                            let idle_area = Rect {
                                x: canvas_x,
                                y: canvas_y,
                                width: IDLE_CANVAS_WIDTH.min(message_area.width),
                                height: IDLE_CANVAS_HEIGHT,
                            };
                            crate::widgets::braille_canvas::BrailleCanvas::new(&app.idle_pattern)
                                .render(idle_area, buf);
                        }
                    }
                }

                if separator_rows > 0 {
                    let line_style = Style::default().fg(app.active_theme.text_dim());
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
                    let border_style = Style::default().fg(app.active_theme.border());
                    crate::render::draw_box(input_area, buf, border_style, None);

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

                // Render the @-file completion popup above the input area.
                if app.file_completion.is_visible() {
                    app.file_completion.render(interactive_area, buf);
                }

                // Render the clarification picker as a centered overlay.
                if app.clarification.is_visible() {
                    app.clarification.render(area, buf);
                }
            }
        }
    }

    // Render tool approval overlay as a modal on top of everything.
    if let Some(approval) = &mut app.approval {
        approval.render(area, buf);
    }

    // Apply post-render effects (tachyonfx).
    app.effects.process(frame_dt, buf, area);

    // Write only changed cells to the terminal, then swap buffers.
    // Synchronized update prevents screen tearing on fast redraws.
    let _ = crossterm::execute!(term.backend_mut(), BeginSynchronizedUpdate);
    let _ = term.draw_diff();
    term.swap_buffers();
    let _ = crossterm::execute!(term.backend_mut(), EndSynchronizedUpdate);
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
        CrosstermEvent::Mouse(mouse) => {
            use crossterm::event::MouseEventKind;
            match mouse.kind {
                // Scroll 1 row per wheel notch for smoother feel (was 3).
                MouseEventKind::ScrollUp => Some(TuiEvent::MouseScroll(1)),
                MouseEventKind::ScrollDown => Some(TuiEvent::MouseScroll(-1)),
                _ => None,
            }
        }
        CrosstermEvent::FocusGained => Some(TuiEvent::FocusGained),
        CrosstermEvent::FocusLost => Some(TuiEvent::FocusLost),
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
        assert!(
            path.is_some(),
            "tui_log_path() should return Some on a system with a home dir"
        );
        let p = path.unwrap();
        assert!(p.ends_with("tui.log"));
        assert!(p.to_string_lossy().contains(".genesis/logs"));
    }
}

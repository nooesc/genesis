//! ChatWidget — composes history cells, active streaming cell, and input widget.
//!
//! Layout (bottom-up within a given area):
//! 1. Bottom 1 row: InputWidget
//! 2. Above that: ActiveCell text (if a turn is running) — word-wrapped with `eve> ` prefix
//! 3. Remaining space: most recent committed cells, filling from the bottom up

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Widget as _, Wrap},
};
use unicode_width::UnicodeWidthStr as _;

use crate::history::agent_cell::{prefix_markdown_lines, AgentCell};
use crate::history::cell::HistoryCell;
use crate::history::tool_cell::{ToolCell, ToolDisplayMode};
use crate::history::user_cell::UserCell;
use crate::widgets::input_widget::InputWidget;

/// A single in-flight tool call tracked during a streaming turn.
pub struct ActiveToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub args_summary: String,
    pub success: Option<bool>,
    pub duration: Option<std::time::Duration>,
}

/// In-flight streaming response, mutable during a turn.
///
/// Accumulates text deltas and tool call start/end events while the agent
/// is responding. Frozen into committed [`HistoryCell`]s by
/// [`ChatWidget::complete_turn`].
pub struct ActiveCell {
    pub text_buffer: String,
    pub tool_calls: Vec<ActiveToolCall>,
}

/// Cache for the active cell's rendered markdown lines.
struct ActiveCellCache {
    /// The `text_buffer.len()` when the cache was last built.
    parsed_len: usize,
    /// The render width when the cache was last built.
    parsed_width: u16,
    /// The cached rendered lines.
    lines: Vec<Line<'static>>,
}

/// Composes committed history cells, an optional active streaming cell,
/// and the input widget into a single renderable unit.
pub struct ChatWidget {
    /// Cells that have been committed (frozen) from previous turns.
    committed_cells: Vec<HistoryCell>,
    /// Cells queued after each committed message/turn.
    ///
    /// Populated by submit/complete paths and drained by
    /// [`drain_pending_scrollback`] when `AppEvent::CommitHistory` is handled.
    pending_scrollback: Vec<HistoryCell>,
    /// The current in-flight turn, if one is running.
    pub active_cell: Option<ActiveCell>,
    /// The text input widget.
    pub input: InputWidget,
    /// Cache for parsed active-cell markdown lines.
    active_cell_cache: Option<ActiveCellCache>,
}

impl ChatWidget {
    /// Create a new, empty `ChatWidget`.
    pub fn new() -> Self {
        Self {
            committed_cells: Vec::new(),
            pending_scrollback: Vec::new(),
            active_cell: None,
            input: InputWidget::new(),
            active_cell_cache: None,
        }
    }

    // ── Turn management ───────────────────────────────────────────────────

    /// Add a user message to committed cells immediately.
    ///
    /// The cell is also queued so the commit path can atomically collect
    /// completed turn cells; alternate-screen mode keeps it in-memory only.
    pub fn add_user_message(&mut self, text: String) {
        let cell = HistoryCell::User(UserCell::new(text));
        self.committed_cells.push(cell.clone());
        self.pending_scrollback.push(cell);
    }

    /// Start a new agent turn — creates an empty [`ActiveCell`].
    ///
    /// If a turn is already running, the existing cell is replaced.
    pub fn start_turn(&mut self) {
        self.active_cell = Some(ActiveCell {
            text_buffer: String::new(),
            tool_calls: Vec::new(),
        });
        self.active_cell_cache = None;
    }

    /// Append streaming text to the active cell's text buffer.
    ///
    /// If no turn is active, this is a no-op.
    pub fn append_text(&mut self, text: &str) {
        if let Some(cell) = &mut self.active_cell {
            cell.text_buffer.push_str(text);
        }
    }

    /// Record a tool call starting in the active cell.
    ///
    /// If no turn is active, this is a no-op.
    pub fn tool_call_start(
        &mut self,
        call_id: String,
        tool_name: String,
        args_summary: String,
    ) {
        if let Some(cell) = &mut self.active_cell {
            cell.tool_calls.push(ActiveToolCall {
                call_id,
                tool_name,
                args_summary,
                success: None,
                duration: None,
            });
        }
    }

    /// Record a tool call completing (success/failure + wall time).
    ///
    /// Looks up the call by `call_id` and updates it in place. If no match
    /// is found (or no turn is active) this is a no-op.
    pub fn tool_call_end(&mut self, call_id: &str, success: bool, duration: std::time::Duration) {
        if let Some(cell) = &mut self.active_cell {
            if let Some(tc) = cell.tool_calls.iter_mut().find(|t| t.call_id == call_id) {
                tc.success = Some(success);
                tc.duration = Some(duration);
            }
        }
    }

    /// Complete the current turn.
    ///
    /// Freezes the active cell into committed [`HistoryCell`]s:
    /// - If the text buffer is non-empty, an [`AgentCell`] is produced.
    /// - For each completed tool call, a [`ToolCell`] is produced.
    ///   Tool calls with no recorded result (partial) are emitted as failures
    ///   with zero duration.
    ///
    /// Returns the newly committed cells and queues them for the commit path.
    /// Alternate-screen mode keeps this in-memory only.
    pub fn complete_turn(&mut self) -> Vec<HistoryCell> {
        let Some(cell) = self.active_cell.take() else {
            return Vec::new();
        };

        let mut new_cells: Vec<HistoryCell> = Vec::new();

        // Agent text response (if any).
        if !cell.text_buffer.is_empty() {
            new_cells.push(HistoryCell::Agent(AgentCell::new(cell.text_buffer)));
        }

        // One ToolCell per tool call.
        for tc in cell.tool_calls {
            let success = tc.success.unwrap_or(false);
            let duration = tc.duration.unwrap_or(std::time::Duration::ZERO);
            new_cells.push(HistoryCell::Tool(
                ToolCell::new(
                    tc.tool_name,
                    tc.call_id,
                    tc.args_summary,
                    success,
                    duration,
                )
                .with_display_mode(ToolDisplayMode::Grouped),
            ));
        }

        self.active_cell_cache = None;
        self.pending_scrollback.extend(new_cells.iter().cloned());
        self.committed_cells.extend(new_cells.iter().cloned());
        new_cells
    }

    /// Drain cells queued for the commit path.
    ///
    /// Returns the pending cells and clears the internal queue. Called from
    /// `run_tui` where both `App` and `CustomTerminal` are accessible.
    pub fn drain_pending_scrollback(&mut self) -> Vec<HistoryCell> {
        std::mem::take(&mut self.pending_scrollback)
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    /// All committed cells in order (oldest first).
    pub fn committed_cells(&self) -> &[HistoryCell] {
        &self.committed_cells
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Render the chat widget into the given area.
    ///
    /// Backwards-compatible default path: reserve the last row for input.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        // Preserve existing behavior for callers that still use this method.
        let input_height = 1.min(area.height);
        let message_area_height = area.height.saturating_sub(input_height);
        let message_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: message_area_height,
        };
        self.render_messages(message_area, buf);

        let input_area = Rect {
            x: area.x,
            y: area.y + message_area_height,
            width: area.width,
            height: input_height,
        };
        self.render_input(input_area, buf, false);
    }

    /// Render only the chat messages (no input row).
    pub fn render_messages(&mut self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut remaining_rows = area.height;
        let mut bottom_y = area.y + area.height;

        // ── Active cell (if any) ───────────────────────────────────────
        // Extract text_len while holding the immutable borrow, then drop it
        // before we potentially update the cache (which requires &mut self).
        let active_text_info: Option<(usize, u16)> = self
            .active_cell
            .as_ref()
            .filter(|a| !a.text_buffer.is_empty())
            .map(|a| (a.text_buffer.len(), area.width));

        if let Some((text_len, width)) = active_text_info {
            // Re-parse markdown only when the buffer or width has changed.
            let needs_reparse = self.active_cell_cache.as_ref().is_none_or(|c| {
                c.parsed_len != text_len || c.parsed_width != width
            });
            if needs_reparse {
                // Re-borrow text_buffer for the actual parse.
                let text = self.active_cell.as_ref().unwrap().text_buffer.clone();
                let lines = active_cell_lines(&text, width);
                self.active_cell_cache = Some(ActiveCellCache {
                    parsed_len: text_len,
                    parsed_width: width,
                    lines,
                });
            }
            let lines = self.active_cell_cache.as_ref().unwrap().lines.clone();
            let wrap_width = width.max(1);
            let cell_height = wrapped_row_count(&lines, wrap_width).max(1);
            let rows_to_use = cell_height.min(remaining_rows);

            if rows_to_use > 0 {
                let cell_area = Rect {
                    x: area.x,
                    y: bottom_y - rows_to_use,
                    width: area.width,
                    height: rows_to_use,
                };

                let skip = cell_height.saturating_sub(rows_to_use);
                let paragraph = Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .scroll((0, skip));
                paragraph.render(cell_area, buf);

                bottom_y -= rows_to_use;
                remaining_rows -= rows_to_use;
            }
        }

        if remaining_rows == 0 {
            return;
        }

        // ── Committed cells (most recent first, bottom-up) ────────────
        // Walk committed cells in reverse and allocate rows.
        let mut cells_to_render: Vec<(u16, &HistoryCell)> = Vec::new();
        let mut used = 0u16;
        for cell in self.committed_cells.iter().rev() {
            let h = cell.height(area.width).max(1);
            if used + h > remaining_rows {
                break;
            }
            cells_to_render.push((h, cell));
            used += h;
        }

        // Render from oldest to newest (reverse the reversed list).
        cells_to_render.reverse();
        let mut row_cursor = bottom_y.saturating_sub(used);
        for (h, cell) in cells_to_render {
            let cell_area = Rect {
                x: area.x,
                y: row_cursor,
                width: area.width,
                height: h,
            };
            cell.render(cell_area, buf);
            row_cursor += h;
        }
    }

    /// Render the input widget in the given row or box.
    ///
    /// `show_cursor` controls whether the input cursor is visible (used while
    /// an agent turn is running).
    pub fn render_input(&self, area: Rect, buf: &mut Buffer, is_turn_running: bool) {
        self.input
            .render_with_state(area, buf, !is_turn_running);
    }
}

impl Default for ChatWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the markdown-rendered [`Line`]s for the active streaming cell.
///
/// Uses the same `eve> ` prefix/indent pattern as [`AgentCell`], with
/// markdown formatting applied so styles appear live as the agent types.
fn active_cell_lines(text: &str, _width: u16) -> Vec<Line<'static>> {
    prefix_markdown_lines(text)
}

fn wrapped_row_count(lines: &[Line<'static>], wrap_width: u16) -> u16 {
    let width = wrap_width.max(1) as usize;
    let mut rows: usize = 0;

    for line in lines {
        let line_width = line
            .spans
            .iter()
            .map(|span| span.content.width())
            .sum::<usize>();
        let wrapped = if line_width == 0 {
            1
        } else {
            (line_width.saturating_sub(1) / width) + 1
        };
        rows = rows.saturating_add(wrapped);
    }

    rows.try_into().unwrap_or(u16::MAX)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_empty() {
        let cw = ChatWidget::new();
        assert!(cw.committed_cells().is_empty());
        assert!(cw.active_cell.is_none());
        assert_eq!(cw.input.text(), "");
    }

    #[test]
    fn add_user_message_adds_to_committed() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello there".to_string());
        assert_eq!(cw.committed_cells().len(), 1);
        match &cw.committed_cells()[0] {
            HistoryCell::User(uc) => assert_eq!(uc.text, "hello there"),
            other => panic!("expected User cell, got {:?}", other),
        }
    }

    #[test]
    fn start_and_complete_turn_flow() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hi".to_string());
        cw.start_turn();
        assert!(cw.active_cell.is_some());

        cw.append_text("Hello, I am Eve.");
        let cells = cw.complete_turn();
        assert!(!cells.is_empty());
        assert!(cw.active_cell.is_none());

        // The AgentCell should be committed (user + agent = 2 cells).
        assert!(cw.committed_cells().len() >= 2);
    }

    #[test]
    fn append_text_accumulates() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Hello");
        cw.append_text(", ");
        cw.append_text("world.");

        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "Hello, world.");
    }

    #[test]
    fn tool_call_tracking() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(500));

        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.tool_calls.len(), 1);
        assert_eq!(active.tool_calls[0].call_id, "c1");
        assert_eq!(active.tool_calls[0].success, Some(true));
        assert_eq!(
            active.tool_calls[0].duration,
            Some(std::time::Duration::from_millis(500))
        );
    }

    #[test]
    fn complete_turn_produces_cells() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("I ran a command.");
        cw.tool_call_start("c1".into(), "shell".into(), "echo hi".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));

        let cells = cw.complete_turn();

        // Should produce: 1 AgentCell + 1 ToolCell
        assert_eq!(cells.len(), 2);
        assert!(matches!(cells[0], HistoryCell::Agent(_)));
        assert!(matches!(cells[1], HistoryCell::Tool(_)));

        // Both should be in committed_cells.
        assert_eq!(cw.committed_cells().len(), 2);

        // Active cell is cleared.
        assert!(cw.active_cell.is_none());
    }

    #[test]
    fn complete_turn_with_no_text_only_tools() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.tool_call_start("t1".into(), "read_file".into(), "path.txt".into());
        cw.tool_call_end("t1", false, std::time::Duration::from_millis(50));

        let cells = cw.complete_turn();
        assert_eq!(cells.len(), 1);
        assert!(matches!(cells[0], HistoryCell::Tool(_)));
    }

    #[test]
    fn user_message_queued_for_scrollback() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello".to_string());

        let pending = cw.drain_pending_scrollback();
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0], HistoryCell::User(_)));
    }

    #[test]
    fn complete_turn_queues_for_scrollback() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("response");
        let _returned = cw.complete_turn();

        let pending = cw.drain_pending_scrollback();
        assert_eq!(pending.len(), 1);
        assert!(matches!(pending[0], HistoryCell::Agent(_)));
    }

    #[test]
    fn drain_pending_scrollback_clears_queue() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hi".to_string());
        cw.start_turn();
        cw.append_text("response");
        cw.complete_turn();

        // First drain returns user + agent cells.
        let pending = cw.drain_pending_scrollback();
        assert_eq!(pending.len(), 2);

        // Second drain returns nothing.
        let pending2 = cw.drain_pending_scrollback();
        assert!(pending2.is_empty());
    }

    #[test]
    fn full_turn_scrollback_includes_user_and_agent() {
        let mut cw = ChatWidget::new();

        // User submits, then agent responds with text + tool call.
        cw.add_user_message("do something".to_string());
        cw.start_turn();
        cw.append_text("Sure, running a tool.");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(200));
        cw.complete_turn();

        let pending = cw.drain_pending_scrollback();
        // User + Agent + Tool = 3 cells
        assert_eq!(pending.len(), 3);
        assert!(matches!(pending[0], HistoryCell::User(_)));
        assert!(matches!(pending[1], HistoryCell::Agent(_)));
        assert!(matches!(pending[2], HistoryCell::Tool(_)));
    }
}

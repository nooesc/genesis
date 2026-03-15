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
    widgets::Widget as _,
};

use crate::history::agent_cell::AgentCell;
use crate::history::cell::HistoryCell;
use crate::history::{render_prefixed_lines, rgb};
use crate::history::tool_cell::ToolCell;
use crate::history::user_cell::UserCell;
use crate::widgets::input_widget::InputWidget;

/// The active streaming cell prefix.
const ACTIVE_PREFIX: &str = "eve> ";

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

/// Composes committed history cells, an optional active streaming cell,
/// and the input widget into a single renderable unit.
pub struct ChatWidget {
    /// Cells that have been committed (frozen) from previous turns.
    committed_cells: Vec<HistoryCell>,
    /// The current in-flight turn, if one is running.
    pub active_cell: Option<ActiveCell>,
    /// The text input widget.
    pub input: InputWidget,
}

impl ChatWidget {
    /// Create a new, empty `ChatWidget`.
    pub fn new() -> Self {
        Self {
            committed_cells: Vec::new(),
            active_cell: None,
            input: InputWidget::new(),
        }
    }

    // ── Turn management ───────────────────────────────────────────────────

    /// Add a user message to committed cells immediately.
    pub fn add_user_message(&mut self, text: String) {
        self.committed_cells
            .push(HistoryCell::User(UserCell::new(text)));
    }

    /// Start a new agent turn — creates an empty [`ActiveCell`].
    ///
    /// If a turn is already running, the existing cell is replaced.
    pub fn start_turn(&mut self) {
        self.active_cell = Some(ActiveCell {
            text_buffer: String::new(),
            tool_calls: Vec::new(),
        });
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
    /// Returns the newly committed cells so callers can push them to
    /// scrollback. Also appends them to `committed_cells`.
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
            new_cells.push(HistoryCell::Tool(ToolCell::new(
                tc.tool_name,
                tc.call_id,
                tc.args_summary,
                success,
                duration,
            )));
        }

        self.committed_cells.extend(new_cells.clone());
        new_cells
    }

    // ── Accessors ─────────────────────────────────────────────────────────

    /// All committed cells in order (oldest first).
    pub fn committed_cells(&self) -> &[HistoryCell] {
        &self.committed_cells
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Render the chat widget into the given area.
    ///
    /// Layout (bottom-up):
    /// 1. Bottom 1 row — InputWidget
    /// 2. Above that — active streaming cell (if running), word-wrapped
    /// 3. Remaining space — most recent committed cells, bottom-aligned
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        // ── 1. Reserve bottom row for input ───────────────────────────────
        let input_row = area.y + area.height - 1;
        let input_area = Rect {
            x: area.x,
            y: input_row,
            width: area.width,
            height: 1,
        };
        self.input.render(input_area, buf);

        if area.height < 2 {
            return;
        }

        // Available rows above the input.
        let mut remaining_rows = area.height - 1;
        let mut bottom_y = input_row; // exclusive: next cell renders above this

        // ── 2. Active cell (if any) ───────────────────────────────────────
        if let Some(active) = &self.active_cell {
            if !active.text_buffer.is_empty() {
                let lines = active_cell_lines(&active.text_buffer, area.width);
                let cell_height = lines.len() as u16;
                let rows_to_use = cell_height.min(remaining_rows);

                if rows_to_use > 0 {
                    let cell_area = Rect {
                        x: area.x,
                        y: bottom_y - rows_to_use,
                        width: area.width,
                        height: rows_to_use,
                    };
                    // Render the last `rows_to_use` lines (clip from top if needed).
                    let skip = lines.len().saturating_sub(rows_to_use as usize);
                    let visible_lines: Vec<Line<'_>> = lines.into_iter().skip(skip).collect();
                    let paragraph = ratatui::widgets::Paragraph::new(visible_lines);
                    paragraph.render(cell_area, buf);

                    bottom_y -= rows_to_use;
                    remaining_rows -= rows_to_use;
                }
            }
        }

        if remaining_rows == 0 {
            return;
        }

        // ── 3. Committed cells (most recent first, bottom-up) ────────────
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
        let mut row_cursor = bottom_y - used;
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
}

impl Default for ChatWidget {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the word-wrapped [`Line`]s for the active streaming cell.
///
/// Uses the same `eve> ` prefix/indent pattern as [`AgentCell`].
fn active_cell_lines(text: &str, width: u16) -> Vec<Line<'static>> {
    render_prefixed_lines(
        text,
        width,
        ACTIVE_PREFIX,
        rgb(genesis_ui::colors::EVE_LAVENDER),
        rgb(genesis_ui::colors::UI_TEXT),
    )
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
}

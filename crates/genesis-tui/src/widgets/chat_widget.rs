//! ChatWidget — composes history cells, active streaming cell, and input widget.
//!
//! Layout (bottom-up within a given area):
//! 1. Bottom 1 row: InputWidget
//! 2. Above that: ActiveCell text (if a turn is running) — word-wrapped with `eve> ` prefix
//! 3. Remaining space: most recent committed cells, filling from the bottom up

use std::collections::HashSet;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Paragraph, Widget as _, Wrap},
};
use unicode_width::UnicodeWidthStr as _;

use crate::history::agent_cell::{prefix_markdown_lines, AgentCell};
use crate::history::cell::HistoryCell;
use crate::history::tool_cell::{
    tool_group_summary_height, tool_group_summary_line, ToolCell, ToolDisplayMode,
};
use crate::history::user_cell::UserCell;
use crate::widgets::input_widget::InputWidget;

/// Minimum number of consecutive tool cells required to form a collapsible group.
const MIN_GROUP_SIZE: usize = 2;

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
    /// Indices of tool groups that are expanded (keyed by `start_idx` in
    /// `committed_cells`). Groups not present here are rendered as a
    /// collapsed single-line summary.
    pub expanded_tool_groups: HashSet<usize>,
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
            expanded_tool_groups: HashSet::new(),
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

    // ── Tool group detection ──────────────────────────────────────────────

    /// Identify ranges of consecutive tool cells that should be grouped.
    ///
    /// Returns `(start_idx, count)` pairs for groups of [`MIN_GROUP_SIZE`]+
    /// consecutive `HistoryCell::Tool` entries in `committed_cells`.
    pub fn find_tool_groups(&self) -> Vec<(usize, usize)> {
        let mut groups = Vec::new();
        let mut i = 0;
        while i < self.committed_cells.len() {
            if matches!(self.committed_cells[i], HistoryCell::Tool(_)) {
                let start = i;
                while i < self.committed_cells.len()
                    && matches!(self.committed_cells[i], HistoryCell::Tool(_))
                {
                    i += 1;
                }
                let count = i - start;
                if count >= MIN_GROUP_SIZE {
                    groups.push((start, count));
                }
            } else {
                i += 1;
            }
        }
        groups
    }

    /// Check if a committed-cell index falls within a tool group.
    ///
    /// Returns `Some((start, count))` if the index is part of a group,
    /// `None` otherwise.
    fn group_containing(idx: usize, groups: &[(usize, usize)]) -> Option<(usize, usize)> {
        groups
            .iter()
            .find(|(start, count)| idx >= *start && idx < start + count)
            .copied()
    }

    /// Compute the total visible content height (committed cells + active cell)
    /// for the given width, without actually rendering anything.
    ///
    /// Returns 0 when there are no cells and no active turn.
    pub fn visible_content_height(&self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }
        let tool_groups = self.find_tool_groups();
        let mut total: u16 = 0;
        let mut prev_is_user: Option<bool> = None;
        let mut i = 0;
        while i < self.committed_cells.len() {
            let cell = &self.committed_cells[i];
            let cur_is_user = matches!(cell, HistoryCell::User(_));

            // Check if this index starts a tool group.
            if let Some((start, count)) = Self::group_containing(i, &tool_groups) {
                // The `i == start` guard is always true here: the start
                // branch advances `i` past the whole group, so we never
                // encounter a mid-group index.
                debug_assert_eq!(i, start);

                // Separator between user turn and this group?
                if let Some(prev) = prev_is_user {
                    if cur_is_user != prev {
                        total = total.saturating_add(1);
                    }
                }
                if self.expanded_tool_groups.contains(&start) {
                    // Expanded: render each tool cell individually.
                    for j in start..start + count {
                        let c = &self.committed_cells[j];
                        total = total.saturating_add(c.height(width).max(1));
                    }
                } else {
                    // Collapsed: single summary line.
                    total =
                        total.saturating_add(tool_group_summary_height(count));
                }
                prev_is_user = Some(false); // tool cells are not user cells
                i = start + count;
                continue;
            }

            if let Some(prev) = prev_is_user {
                if cur_is_user != prev {
                    // Turn boundary separator row.
                    total = total.saturating_add(1);
                }
            }
            prev_is_user = Some(cur_is_user);
            total = total.saturating_add(cell.height(width).max(1));
            i += 1;
        }
        if let Some(active) = &self.active_cell {
            if !active.text_buffer.is_empty() {
                // Account for a separator before the active cell if last committed
                // cell was a User cell (active cell is always an agent response).
                if prev_is_user == Some(true) {
                    total = total.saturating_add(1);
                }
                // Rough estimate: use line count based on prefix_markdown_lines.
                let lines =
                    crate::history::agent_cell::prefix_markdown_lines(&active.text_buffer);
                let h = wrapped_row_count(&lines, width).max(1);
                total = total.saturating_add(h);
            }
        }
        total
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
        // Walk committed cells in reverse and allocate rows, including
        // 1-row turn separators between user and agent/tool groups.
        //
        // Tool groups (2+ consecutive Tool cells) are collapsed into a
        // single summary line unless expanded via `expanded_tool_groups`.
        //
        // `needs_sep` tracks whether a separator is needed *above* the
        // most-recently-collected cell. We look one cell further back
        // and check whether the turn type differs.

        /// A renderable entry in the bottom-up layout pass.
        enum RenderEntry<'a> {
            /// A single history cell rendered normally.
            Single {
                height: u16,
                cell: &'a HistoryCell,
                /// True when a separator should be rendered *below* this entry
                /// (i.e. between this entry and the next-newer one).
                separator_after: bool,
            },
            /// A collapsed tool group summary (replaces N tool cells).
            GroupSummary {
                height: u16,
                /// The tool cells in this group (oldest first).
                tools: Vec<&'a ToolCell>,
                separator_after: bool,
            },
        }

        let tool_groups = self.find_tool_groups();

        let mut entries: Vec<RenderEntry<'_>> = Vec::new();
        let mut used = 0u16;
        let mut prev_is_user: Option<bool> = None;

        // Reverse walk index.
        let mut ri = self.committed_cells.len();
        while ri > 0 {
            ri -= 1;
            let cell = &self.committed_cells[ri];
            let cur_is_user = matches!(cell, HistoryCell::User(_));

            // Check if this cell is part of a tool group.
            if let Some((start, count)) = Self::group_containing(ri, &tool_groups) {
                // When walking in reverse, we first encounter the *last* cell
                // of the group. Process the entire group at once from the
                // group's start index.
                if ri != start + count - 1 {
                    // Not the last element — we already processed this group.
                    continue;
                }

                let needs_sep =
                    prev_is_user.is_some_and(|prev| prev != cur_is_user);

                if self.expanded_tool_groups.contains(&start) {
                    // Expanded: emit each tool cell individually (in reverse).
                    let mut temp: Vec<RenderEntry<'_>> = Vec::new();
                    for j in (start..start + count).rev() {
                        let tc = &self.committed_cells[j];
                        let h = tc.height(area.width).max(1);
                        temp.push(RenderEntry::Single {
                            height: h,
                            cell: tc,
                            separator_after: false,
                        });
                    }
                    // In the reverse walk, temp[0] is the newest entry.
                    // After entries.reverse(), it becomes the last entry —
                    // which is where the separator belongs (between this
                    // group and the next-newer cell collected earlier).
                    if let Some(RenderEntry::Single {
                        separator_after, ..
                    }) = temp.first_mut()
                    {
                        *separator_after = needs_sep;
                    }
                    // Fit as many individual cells from the group as the
                    // budget allows, instead of dropping the entire group.
                    let sep_cost = if needs_sep { 1u16 } else { 0 };
                    if used + sep_cost > remaining_rows {
                        break;
                    }
                    let mut fitted = 0u16;
                    let mut accepted = 0usize;
                    for t in &temp {
                        let th = match t {
                            RenderEntry::Single { height, .. } => *height,
                            RenderEntry::GroupSummary { height, .. } => *height,
                        };
                        if used + sep_cost + fitted + th > remaining_rows {
                            break;
                        }
                        fitted += th;
                        accepted += 1;
                    }
                    if accepted == 0 {
                        break;
                    }
                    temp.truncate(accepted);
                    entries.extend(temp);
                    used += sep_cost + fitted;
                } else {
                    // Collapsed: single summary line.
                    let h = tool_group_summary_height(count);
                    let cost = h + if needs_sep { 1 } else { 0 };
                    if used + cost > remaining_rows {
                        break;
                    }
                    // Collect ToolCell refs for the summary renderer.
                    let tools: Vec<&ToolCell> = (start..start + count)
                        .filter_map(|j| {
                            if let HistoryCell::Tool(tc) = &self.committed_cells[j]
                            {
                                Some(tc)
                            } else {
                                None
                            }
                        })
                        .collect();
                    entries.push(RenderEntry::GroupSummary {
                        height: h,
                        tools,
                        separator_after: needs_sep,
                    });
                    used += cost;
                }

                prev_is_user = Some(false); // tools are agent-side
                // Skip past the rest of the group.
                ri = start;
                continue;
            }

            // Normal (non-grouped) cell.
            let h = cell.height(area.width).max(1);
            let needs_sep =
                prev_is_user.is_some_and(|prev| prev != cur_is_user);
            let cost = h + if needs_sep { 1 } else { 0 };
            if used + cost > remaining_rows {
                break;
            }
            entries.push(RenderEntry::Single {
                height: h,
                cell,
                separator_after: needs_sep,
            });
            used += cost;
            prev_is_user = Some(cur_is_user);
        }

        // Also account for a separator between the last committed cell
        // and the active cell, if the active cell was rendered above.
        let active_sep = if active_text_info.is_some() {
            // Active cell is always an agent response. If the last committed
            // cell (first in `entries` since entries is newest-first) is a
            // User cell, we need a separator.
            entries.first().is_some_and(|e| match e {
                RenderEntry::Single { cell, .. } => {
                    matches!(cell, HistoryCell::User(_))
                }
                RenderEntry::GroupSummary { .. } => false,
            })
        } else {
            false
        };
        if active_sep && used < remaining_rows {
            used += 1;
        }

        // Render from oldest to newest (reverse the reversed list).
        entries.reverse();
        let mut row_cursor = bottom_y.saturating_sub(used);

        let sep_style =
            Style::default().fg(crate::history::rgb(genesis_ui::colors::UI_DIM));

        for entry in &entries {
            match entry {
                RenderEntry::Single {
                    height,
                    cell,
                    separator_after,
                } => {
                    let cell_area = Rect {
                        x: area.x,
                        y: row_cursor,
                        width: area.width,
                        height: *height,
                    };
                    cell.render(cell_area, buf);
                    row_cursor += height;
                    if *separator_after {
                        render_turn_separator(
                            area.x,
                            row_cursor,
                            area.width,
                            sep_style,
                            buf,
                        );
                        row_cursor += 1;
                    }
                }
                RenderEntry::GroupSummary {
                    height,
                    tools,
                    separator_after,
                } => {
                    let lines = tool_group_summary_line(tools);
                    let paragraph = Paragraph::new(lines);
                    let cell_area = Rect {
                        x: area.x,
                        y: row_cursor,
                        width: area.width,
                        height: *height,
                    };
                    paragraph.render(cell_area, buf);
                    row_cursor += height;
                    if *separator_after {
                        render_turn_separator(
                            area.x,
                            row_cursor,
                            area.width,
                            sep_style,
                            buf,
                        );
                        row_cursor += 1;
                    }
                }
            }
        }

        // Render a separator between the last committed cell and the
        // active streaming cell, if needed.
        if active_sep {
            render_turn_separator(area.x, row_cursor, area.width, sep_style, buf);
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

/// Draw a dim horizontal `─` separator across a single row.
fn render_turn_separator(x: u16, y: u16, width: u16, style: Style, buf: &mut Buffer) {
    for col in x..x + width {
        if let Some(cell) = buf.cell_mut((col, y)) {
            cell.set_symbol("─");
            cell.set_style(style);
        }
    }
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

    #[test]
    fn visible_content_height_returns_zero_for_empty_widget() {
        let cw = ChatWidget::new();
        assert_eq!(cw.visible_content_height(80), 0);
    }

    #[test]
    fn visible_content_height_returns_zero_for_zero_width() {
        let cw = ChatWidget::new();
        assert_eq!(cw.visible_content_height(0), 0);
    }

    #[test]
    fn visible_content_height_counts_committed_cells() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello".to_string());
        let h = cw.visible_content_height(80);
        assert!(h >= 1, "should have at least 1 row for user message, got {h}");
    }

    #[test]
    fn visible_content_height_includes_turn_separators() {
        let mut cw = ChatWidget::new();
        // User message (1 row) + separator (1 row) + agent response (1 row) = 3
        cw.add_user_message("hello".to_string());
        cw.start_turn();
        cw.append_text("hi");
        cw.complete_turn();

        let h = cw.visible_content_height(80);
        // User(1) + separator(1) + Agent(1) = 3
        assert_eq!(h, 3, "should include separator between user and agent, got {h}");
    }

    #[test]
    fn visible_content_height_no_separator_between_same_turn_type() {
        let mut cw = ChatWidget::new();
        // Two consecutive agent cells (no user cell between them)
        cw.start_turn();
        cw.append_text("first response");
        cw.complete_turn();
        cw.start_turn();
        cw.append_text("second response");
        cw.complete_turn();

        let h = cw.visible_content_height(80);
        // Agent(1) + Agent(1) = 2, no separator between same types
        assert_eq!(h, 2, "no separator between same turn types, got {h}");
    }

    #[test]
    fn render_messages_draws_separator_between_turns() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hi".to_string());
        cw.start_turn();
        cw.append_text("hello");
        cw.complete_turn();

        // Render into a buffer tall enough for all content.
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // The separator should contain '─' characters somewhere in the buffer.
        let has_separator = (0..area.height).any(|row| {
            (0..area.width).all(|col| {
                buf.cell((col, row))
                    .map_or(false, |c| c.symbol() == "─")
            })
        });
        assert!(
            has_separator,
            "should render a horizontal separator between user and agent turns"
        );
    }

    #[test]
    fn render_messages_no_separator_between_tool_cells() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Running tools.");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        // Agent + Tool + Tool = 3 cells, all in the same agent turn.
        // No separator should appear (all are agent-side).
        let area = Rect::new(0, 0, 40, 20);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // Count rows that are entirely '─' characters (full-width separators).
        let separator_count = (0..area.height)
            .filter(|&row| {
                (0..area.width).all(|col| {
                    buf.cell((col, row))
                        .map_or(false, |c| c.symbol() == "─")
                })
            })
            .count();
        assert_eq!(
            separator_count, 0,
            "no turn separator between consecutive agent/tool cells"
        );
    }

    // ── Tool group tests ──────────────────────────────────────────────────

    /// Helper: build a widget with an agent cell followed by N tool cells.
    fn widget_with_tools(tool_count: usize) -> ChatWidget {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Running tools.");
        for i in 0..tool_count {
            let id = format!("c{i}");
            cw.tool_call_start(id.clone(), "shell".into(), format!("cmd{i}"));
            cw.tool_call_end(&id, true, std::time::Duration::from_millis(100));
        }
        cw.complete_turn();
        cw
    }

    #[test]
    fn find_tool_groups_no_tools() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hi".to_string());
        assert!(cw.find_tool_groups().is_empty());
    }

    #[test]
    fn find_tool_groups_single_tool_no_group() {
        let cw = widget_with_tools(1);
        // 1 tool cell is below the threshold — no group.
        assert!(
            cw.find_tool_groups().is_empty(),
            "a single tool cell should not form a group"
        );
    }

    #[test]
    fn find_tool_groups_two_tools_forms_group() {
        let cw = widget_with_tools(2);
        let groups = cw.find_tool_groups();
        assert_eq!(groups.len(), 1, "two consecutive tools should form one group");
        let (start, count) = groups[0];
        assert_eq!(count, 2);
        // The agent cell is at index 0, tools at 1 and 2.
        assert_eq!(start, 1);
    }

    #[test]
    fn find_tool_groups_three_tools_forms_group() {
        let cw = widget_with_tools(3);
        let groups = cw.find_tool_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], (1, 3));
    }

    #[test]
    fn find_tool_groups_separated_by_agent() {
        let mut cw = ChatWidget::new();
        // Turn 1: 2 tools
        cw.start_turn();
        cw.tool_call_start("a".into(), "shell".into(), "ls".into());
        cw.tool_call_end("a", true, std::time::Duration::from_millis(50));
        cw.tool_call_start("b".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("b", true, std::time::Duration::from_millis(50));
        cw.complete_turn();
        // Turn 2: agent text + 2 tools
        cw.start_turn();
        cw.append_text("more");
        cw.tool_call_start("c".into(), "grep".into(), "foo".into());
        cw.tool_call_end("c", true, std::time::Duration::from_millis(50));
        cw.tool_call_start("d".into(), "grep".into(), "bar".into());
        cw.tool_call_end("d", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        let groups = cw.find_tool_groups();
        assert_eq!(groups.len(), 2, "should have two separate tool groups");
    }

    #[test]
    fn collapsed_group_reduces_visible_height() {
        let cw = widget_with_tools(3);
        // With grouping (collapsed by default): agent(1) + summary(1) = 2
        let collapsed_h = cw.visible_content_height(80);

        let mut cw_expanded = widget_with_tools(3);
        // Expand the group to compare.
        let groups = cw_expanded.find_tool_groups();
        cw_expanded.expanded_tool_groups.insert(groups[0].0);
        let expanded_h = cw_expanded.visible_content_height(80);

        assert!(
            collapsed_h < expanded_h,
            "collapsed height ({collapsed_h}) should be less than expanded ({expanded_h})"
        );
    }

    #[test]
    fn expanded_tool_groups_field_starts_empty() {
        let cw = ChatWidget::new();
        assert!(cw.expanded_tool_groups.is_empty());
    }

    #[test]
    fn render_collapsed_group_shows_summary() {
        let mut cw = widget_with_tools(3);
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // The summary line should contain the triangle marker.
        let has_triangle = (0..area.height).any(|row| {
            (0..area.width).any(|col| {
                buf.cell((col, row))
                    .map_or(false, |c| c.symbol() == "\u{25b8}")
            })
        });
        assert!(
            has_triangle,
            "collapsed group should render the triangle marker"
        );
    }

    #[test]
    fn render_expanded_group_shows_individual_tools() {
        let mut cw = widget_with_tools(3);
        let groups = cw.find_tool_groups();
        cw.expanded_tool_groups.insert(groups[0].0);

        let area = Rect::new(0, 0, 60, 30);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // Should NOT contain the triangle (group is expanded).
        let has_triangle = (0..area.height).any(|row| {
            (0..area.width).any(|col| {
                buf.cell((col, row))
                    .map_or(false, |c| c.symbol() == "\u{25b8}")
            })
        });
        assert!(
            !has_triangle,
            "expanded group should not render the summary triangle"
        );

        // Should contain tool border characters (from grouped tool cells).
        let has_border = (0..area.height).any(|row| {
            (0..area.width).any(|col| {
                buf.cell((col, row))
                    .map_or(false, |c| c.symbol() == "\u{250c}")
            })
        });
        assert!(
            has_border,
            "expanded group should render individual tool cell borders"
        );
    }
}

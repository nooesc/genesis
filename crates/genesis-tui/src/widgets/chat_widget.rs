//! ChatWidget — composes history cells, active streaming cell, and input widget.
//!
//! Layout (bottom-up within a given area):
//! 1. Bottom 1 row: InputWidget
//! 2. Above that: ActiveCell text (if a turn is running) — word-wrapped with `eve> ` prefix
//! 3. Remaining space: most recent committed cells, filling from the bottom up

use std::collections::HashSet;

use crate::history::agent_cell::{prefix_markdown_lines, AgentCell};
use crate::history::cell::HistoryCell;
use crate::history::tool_cell::{tool_group_summary_line, ToolCell, ToolDisplayMode};
use crate::history::user_cell::UserCell;
use crate::streaming::StreamingBuffer;
use crate::widgets::input_widget::InputWidget;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget as _, Wrap},
};

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

/// A visual entry in the rendered message list — either a normal cell
/// or a collapsed tool group summary.
enum VisualEntry<'a> {
    /// A single committed cell rendered normally.
    Cell(&'a HistoryCell),
    /// A collapsed tool group summary line.
    GroupSummary(Line<'static>),
}

/// A row-allocated entry ready for rendering, with its pre-computed height
/// and whether a turn separator follows it.
struct RowEntry<'a> {
    height: u16,
    visual: VisualEntry<'a>,
    /// True when a separator should be rendered *below* this entry.
    separator_after: bool,
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
    /// Indices of tool groups that are currently expanded (showing individual cells).
    ///
    /// A tool group is identified by the index of its first cell in `committed_cells`.
    /// Groups not present in this set are rendered as a single collapsed summary line.
    expanded_tool_groups: HashSet<usize>,
    /// Theme-derived accent color (for cursor, active indicators).
    theme_accent: ratatui::style::Color,
    /// Theme-derived dim color (for separators, hints).
    theme_dim: ratatui::style::Color,
    /// Number of rows scrolled back from the bottom (0 = pinned to bottom).
    scroll_offset: usize,
    /// True when the user has scrolled up and auto-scroll is disabled.
    scroll_locked: bool,
    /// Streaming animation buffer — buffers incoming text deltas and releases
    /// them line-by-line for smooth typewriter-like animation.
    streaming_buffer: StreamingBuffer,
    /// Monotonically increasing counter bumped on cell add/complete/expand.
    /// Used to invalidate cached height computations.
    revision: u64,
    /// Cached result of `committed_content_height(width)`.
    committed_height_cache: Option<(u64, u16, usize)>, // (revision, width, height)
    /// Cached tool groups result from `find_tool_groups()`.
    tool_groups_cache: Option<(u64, Vec<(usize, usize)>)>, // (revision, groups)
    /// The last agent response text (for `/copy` command).
    last_copyable_output: Option<String>,
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
            theme_accent: crate::history::rgb(genesis_ui::colors::EVE_LAVENDER),
            theme_dim: crate::history::rgb(genesis_ui::colors::UI_DIM),
            scroll_offset: 0,
            scroll_locked: false,
            streaming_buffer: StreamingBuffer::new(),
            revision: 0,
            committed_height_cache: None,
            tool_groups_cache: None,
            last_copyable_output: None,
        }
    }

    // ── Theme ─────────────────────────────────────────────────────────────

    /// Update theme-derived colors for the chat view.
    pub fn set_theme(&mut self, theme: &dyn crate::theme::Theme) {
        self.theme_accent = theme.primary();
        self.theme_dim = theme.text_dim();
    }

    // ── Scroll ────────────────────────────────────────────────────────────

    /// Compute the maximum allowed scroll offset for the current content
    /// at the given viewport width. Only counts committed cells (not the
    /// active streaming cell) because `render_messages` hides the active
    /// cell when the user has scrolled up.
    fn scroll_max(&mut self, viewport_width: u16) -> usize {
        self.committed_content_height(viewport_width)
    }

    /// Scroll the chat view up by `rows` rows, clamped to the visible
    /// content height so the offset never grows unbounded.
    pub fn scroll_up(&mut self, rows: usize, viewport_width: u16) {
        let max = self.scroll_max(viewport_width);
        self.scroll_offset = self.scroll_offset.saturating_add(rows).min(max);
        self.scroll_locked = true;
    }

    /// Scroll the chat view down by `rows` rows. Re-enables auto-scroll
    /// when the offset reaches zero.
    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(rows);
        if self.scroll_offset == 0 {
            self.scroll_locked = false;
        }
    }

    /// Jump to the bottom and re-enable auto-scroll.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.scroll_locked = false;
    }

    /// Whether the user has scrolled up from the bottom.
    pub fn is_scrolled_up(&self) -> bool {
        self.scroll_locked
    }

    /// Reclamp the scroll offset after a resize or content change.
    ///
    /// When the terminal width changes, wrapped content takes a different
    /// number of rows. If the old offset now exceeds the new maximum,
    /// this brings it back in range (or snaps to bottom if it would be 0).
    pub fn reclamp_scroll(&mut self, viewport_width: u16) {
        if !self.scroll_locked {
            return;
        }
        let max = self.scroll_max(viewport_width);
        if self.scroll_offset > max {
            self.scroll_offset = max;
        }
        if self.scroll_offset == 0 {
            self.scroll_locked = false;
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
        self.bump_revision();
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
        self.streaming_buffer.reset();
    }

    /// Append streaming text to the active cell via the streaming buffer.
    ///
    /// Text is buffered until newlines are found, then released line-by-line
    /// by [`tick_streaming`] for smooth typewriter-like animation. If no turn
    /// is active, this is a no-op.
    ///
    /// Returns `true` if new lines were enqueued (caller should ensure the
    /// streaming tick timer is running).
    pub fn append_text(&mut self, text: &str) -> bool {
        if self.active_cell.is_none() {
            return false;
        }
        self.streaming_buffer.push_delta(text)
    }

    /// Drain buffered streaming text and commit it to the active cell.
    ///
    /// Called once per frame tick. In smooth mode, commits one line. In
    /// catch-up mode, commits all queued lines at once.
    ///
    /// Returns `true` if text was committed (frame needs redraw).
    pub fn tick_streaming(&mut self) -> bool {
        if let Some(text) = self.streaming_buffer.tick() {
            if let Some(cell) = &mut self.active_cell {
                cell.text_buffer.push_str(&text);
                return true;
            }
        }
        false
    }

    /// Whether the streaming buffer has complete lines ready to animate.
    ///
    /// Used by the frame scheduler to decide whether to keep the 120fps
    /// animation timer running. Only returns true when there are actual
    /// lines in the queue; partial (no-newline) text does NOT keep the
    /// timer spinning.
    pub fn has_streaming_pending(&self) -> bool {
        self.streaming_buffer.has_queued_lines()
    }

    /// Return the full visible text for the active cell: committed text
    /// plus buffered preview (queued lines and partial pending text).
    ///
    /// This ensures the user always sees streaming content as it arrives,
    /// even before newlines trigger the line-commit animation.
    pub(crate) fn active_cell_preview_text(&self) -> Option<String> {
        let cell = self.active_cell.as_ref()?;
        let preview = self.streaming_buffer.preview();
        if preview.is_empty() {
            if cell.text_buffer.is_empty() {
                None
            } else {
                Some(cell.text_buffer.clone())
            }
        } else {
            Some(format!("{}{}", cell.text_buffer, preview))
        }
    }

    /// Record a tool call starting in the active cell.
    ///
    /// If no turn is active, this is a no-op.
    pub fn tool_call_start(&mut self, call_id: String, tool_name: String, args_summary: String) {
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
        // Flush any remaining buffered streaming text before freezing the cell.
        if let Some(remaining) = self.streaming_buffer.finalize() {
            if let Some(cell) = &mut self.active_cell {
                cell.text_buffer.push_str(&remaining);
            }
        }

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
                ToolCell::new(tc.tool_name, tc.call_id, tc.args_summary, success, duration)
                    .with_display_mode(ToolDisplayMode::Grouped),
            ));
        }

        // Store the last agent text response for /copy.
        if let Some(agent_cell) = new_cells.iter().find_map(|c| match c {
            HistoryCell::Agent(a) => Some(a),
            _ => None,
        }) {
            self.last_copyable_output = Some(agent_cell.text().to_string());
        }

        self.active_cell_cache = None;
        self.pending_scrollback.extend(new_cells.iter().cloned());
        self.committed_cells.extend(new_cells.iter().cloned());
        self.bump_revision();
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

    /// Change the display mode for all existing tool cells.
    pub fn set_tool_display(&mut self, mode: crate::history::tool_cell::ToolDisplayMode) {
        for cell in &mut self.committed_cells {
            if let HistoryCell::Tool(tc) = cell {
                tc.set_display_mode(mode);
            }
        }
        self.bump_revision();
    }

    /// Bump the revision counter, invalidating all caches.
    fn bump_revision(&mut self) {
        self.revision += 1;
    }

    /// Find runs of 2+ consecutive `HistoryCell::Tool` entries.
    ///
    /// Returns `(start_idx, count)` pairs where `start_idx` is the index
    /// in `committed_cells` and `count` is how many consecutive Tool cells
    /// belong to the group. Cached by revision.
    pub fn find_tool_groups(&mut self) -> Vec<(usize, usize)> {
        if let Some((rev, ref groups)) = self.tool_groups_cache {
            if rev == self.revision {
                return groups.clone();
            }
        }
        let groups = self.compute_tool_groups();
        self.tool_groups_cache = Some((self.revision, groups.clone()));
        groups
    }

    /// Uncached tool group computation.
    fn compute_tool_groups(&self) -> Vec<(usize, usize)> {
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
                if count >= 2 {
                    groups.push((start, count));
                }
            } else {
                i += 1;
            }
        }
        groups
    }

    /// The last agent response text, suitable for clipboard copy.
    pub fn last_copyable(&self) -> Option<&str> {
        self.last_copyable_output.as_deref()
    }

    /// Expand a tool group (show individual cells instead of summary).
    pub fn expand_tool_group(&mut self, group_start: usize) {
        if self.expanded_tool_groups.insert(group_start) {
            self.bump_revision();
        }
    }

    /// Collapse a tool group back to a summary line.
    pub fn collapse_tool_group(&mut self, group_start: usize) {
        if self.expanded_tool_groups.remove(&group_start) {
            self.bump_revision();
        }
    }

    /// Whether a tool group is currently expanded.
    pub fn is_tool_group_expanded(&self, group_start: usize) -> bool {
        self.expanded_tool_groups.contains(&group_start)
    }

    /// Compute the total visible content height (committed cells + active cell)
    /// for the given width, without actually rendering anything.
    ///
    /// Returns 0 when there are no cells and no active turn.
    pub fn visible_content_height(&mut self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }

        // Build a set of cell indices that belong to collapsed tool groups.
        let tool_groups = self.find_tool_groups();
        let mut collapsed_indices: HashSet<usize> = HashSet::new();
        let mut collapsed_group_starts: HashSet<usize> = HashSet::new();
        for &(start, count) in &tool_groups {
            if !self.expanded_tool_groups.contains(&start) {
                for idx in start..start + count {
                    collapsed_indices.insert(idx);
                }
                collapsed_group_starts.insert(start);
            }
        }

        let mut total: u16 = 0;
        let mut prev_is_user: Option<bool> = None;
        let mut i = 0;
        while i < self.committed_cells.len() {
            if collapsed_indices.contains(&i) && collapsed_group_starts.contains(&i) {
                // This is the start of a collapsed group -- count as 1 row.
                let cur_is_user = false; // Tool cells are not user cells.
                if let Some(prev) = prev_is_user {
                    if cur_is_user != prev {
                        total = total.saturating_add(1);
                    }
                }
                prev_is_user = Some(cur_is_user);
                total = total.saturating_add(1); // summary line = 1 row
                                                 // Skip to end of group.
                let count = tool_groups
                    .iter()
                    .find(|&&(s, _)| s == i)
                    .map(|&(_, c)| c)
                    .unwrap_or(1);
                i += count;
                continue;
            } else if collapsed_indices.contains(&i) {
                // Middle/end of a collapsed group -- skip.
                i += 1;
                continue;
            }

            let cell = &self.committed_cells[i];
            let cur_is_user = matches!(cell, HistoryCell::User(_));
            if let Some(prev) = prev_is_user {
                if cur_is_user != prev {
                    total = total.saturating_add(1);
                }
            }
            prev_is_user = Some(cur_is_user);
            total = total.saturating_add(cell.height(width).max(1));
            i += 1;
        }
        // Use preview text (committed + buffered) for height calculation
        // so it accounts for text not yet committed by the streaming buffer.
        if let Some(preview) = self.active_cell_preview_text() {
            if !preview.is_empty() {
                // Account for a separator before the active cell if last committed
                // cell was a User cell (active cell is always an agent response).
                if prev_is_user == Some(true) {
                    total = total.saturating_add(1);
                }
                // Reuse the active cell cache when possible to avoid redundant
                // markdown re-parsing.
                let h = if let Some(cache) = self.active_cell_cache.as_ref() {
                    if cache.parsed_len == preview.len() && cache.parsed_width == width {
                        wrapped_row_count(&cache.lines, width).max(1)
                    } else {
                        let lines = crate::history::agent_cell::prefix_markdown_lines(&preview);
                        wrapped_row_count(&lines, width).max(1)
                    }
                } else {
                    let lines = crate::history::agent_cell::prefix_markdown_lines(&preview);
                    wrapped_row_count(&lines, width).max(1)
                };
                total = total.saturating_add(h);
            }
        }
        total
    }

    /// Compute the total height of committed cells only (no active cell),
    /// accounting for collapsed tool groups and turn separators.
    ///
    /// Used by [`scroll_max`] because the active streaming cell is hidden
    /// when the user scrolls up.
    fn committed_content_height(&mut self, width: u16) -> usize {
        if width == 0 {
            return 0;
        }

        // Check cache first.
        if let Some((rev, cached_width, cached_height)) = self.committed_height_cache {
            if rev == self.revision && cached_width == width {
                return cached_height;
            }
        }

        let tool_groups = self.find_tool_groups();
        let mut collapsed_indices: HashSet<usize> = HashSet::new();
        let mut collapsed_group_starts: HashSet<usize> = HashSet::new();
        for &(start, count) in &tool_groups {
            if !self.expanded_tool_groups.contains(&start) {
                for idx in start..start + count {
                    collapsed_indices.insert(idx);
                }
                collapsed_group_starts.insert(start);
            }
        }

        let mut total: usize = 0;
        let mut prev_is_user: Option<bool> = None;
        let mut i = 0;
        while i < self.committed_cells.len() {
            if collapsed_indices.contains(&i) && collapsed_group_starts.contains(&i) {
                let cur_is_user = false;
                if let Some(prev) = prev_is_user {
                    if cur_is_user != prev {
                        total += 1;
                    }
                }
                prev_is_user = Some(cur_is_user);
                total += 1;
                let count = tool_groups
                    .iter()
                    .find(|&&(s, _)| s == i)
                    .map(|&(_, c)| c)
                    .unwrap_or(1);
                i += count;
                continue;
            } else if collapsed_indices.contains(&i) {
                i += 1;
                continue;
            }

            let cell = &self.committed_cells[i];
            let cur_is_user = matches!(cell, HistoryCell::User(_));
            if let Some(prev) = prev_is_user {
                if cur_is_user != prev {
                    total += 1;
                }
            }
            prev_is_user = Some(cur_is_user);
            total += cell.height(width).max(1) as usize;
            i += 1;
        }
        self.committed_height_cache = Some((self.revision, width, total));
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
        let bottom_y = area.y + area.height;
        let mut active_cell_rows: u16 = 0;

        // ── Active cell (if any) ───────────────────────────────────────
        // Only show the active streaming cell when not scrolled up.
        // Use the preview text (committed + buffered) so the user sees
        // streaming content as it arrives, even before newlines trigger
        // the line-commit animation.
        let active_preview: Option<String> = if !self.scroll_locked {
            self.active_cell_preview_text()
        } else {
            None
        };
        let active_text_info: Option<(usize, u16)> =
            active_preview.as_ref().map(|t| (t.len(), area.width));

        if let Some((text_len, width)) = active_text_info {
            // Re-parse markdown only when the preview or width has changed.
            let needs_reparse = self
                .active_cell_cache
                .as_ref()
                .is_none_or(|c| c.parsed_len != text_len || c.parsed_width != width);
            if needs_reparse {
                let lines = active_cell_lines(active_preview.as_ref().unwrap(), width);
                self.active_cell_cache = Some(ActiveCellCache {
                    parsed_len: text_len,
                    parsed_width: width,
                    lines,
                });
            }
            let mut lines = self.active_cell_cache.as_ref().unwrap().lines.clone();

            // Append a block cursor to the last line to indicate streaming.
            let cursor_style = Style::default().fg(self.theme_accent);
            if let Some(last_line) = lines.last_mut() {
                last_line.spans.push(Span::styled("\u{258D}", cursor_style));
            }

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
                    .scroll((skip, 0));
                paragraph.render(cell_area, buf);

                active_cell_rows = rows_to_use;
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
        // single summary line unless the group is in `expanded_tool_groups`.
        //
        // `needs_sep` tracks whether a separator is needed *above* the
        // most-recently-collected cell. We look one cell further back
        // and check whether the turn type differs.

        // Pre-compute tool groups: map each cell index to its group start
        // (only for cells belonging to a group of 2+ consecutive tools).
        let tool_groups = self.find_tool_groups();
        let mut group_of: std::collections::HashMap<usize, usize> =
            std::collections::HashMap::new();
        for &(start, count) in &tool_groups {
            for idx in start..start + count {
                group_of.insert(idx, start);
            }
        }
        // For each group, store the count for quick lookup.
        let group_count: std::collections::HashMap<usize, usize> =
            tool_groups.iter().copied().collect();

        let mut entries: Vec<RowEntry<'_>> = Vec::new();
        let mut used = 0u16;
        let mut prev_is_user: Option<bool> = None;
        let mut cells_collected = 0usize;
        let num_cells = self.committed_cells.len();

        let mut i = num_cells;
        while i > 0 {
            i -= 1;
            let cell = &self.committed_cells[i];

            // Check if this cell belongs to a collapsed tool group.
            if let Some(&group_start) = group_of.get(&i) {
                let count = group_count[&group_start];
                let is_expanded = self.expanded_tool_groups.contains(&group_start);

                if !is_expanded {
                    // Only process once per group -- when we encounter the
                    // *last* cell (highest index), which comes first in the
                    // reverse walk.
                    if i == group_start + count - 1 {
                        // Build summary line from all cells in this group.
                        let tool_cells: Vec<&ToolCell> = (group_start..group_start + count)
                            .filter_map(|idx| match &self.committed_cells[idx] {
                                HistoryCell::Tool(tc) => Some(tc),
                                _ => None,
                            })
                            .collect();
                        let summary = tool_group_summary_line(&tool_cells);
                        let h: u16 = 1; // summary is always 1 row

                        // Tool cells are not User cells, so cur_is_user = false.
                        let cur_is_user = false;
                        let needs_sep = prev_is_user.is_some_and(|prev| prev != cur_is_user);
                        let cost = h + if needs_sep { 1 } else { 0 };
                        if used + cost > remaining_rows {
                            break;
                        }

                        entries.push(RowEntry {
                            height: h,
                            visual: VisualEntry::GroupSummary(summary),
                            separator_after: needs_sep,
                        });
                        used += cost;
                        cells_collected += count;
                        prev_is_user = Some(cur_is_user);

                        // Skip the remaining cells of this group.
                        i = group_start;
                        continue;
                    } else {
                        // Part of a collapsed group but not the last cell --
                        // skip it (the group was already handled or will be
                        // handled when we reach the last cell).
                        continue;
                    }
                }
                // If expanded, fall through to normal per-cell rendering.
            }

            let cur_is_user = matches!(cell, HistoryCell::User(_));
            let h = cell.height(area.width).max(1);

            // Check whether a separator is needed between this cell and
            // the one we collected just before (which is *newer* since we
            // walk in reverse). The separator row is charged to the total
            // height budget.
            let needs_sep = prev_is_user.is_some_and(|prev| prev != cur_is_user);
            let cost = h + if needs_sep { 1 } else { 0 };
            if used + cost > remaining_rows {
                break;
            }

            // The separator conceptually sits between the current cell and
            // the previous (newer) one. We mark the current cell as having
            // a separator after it.
            entries.push(RowEntry {
                height: h,
                visual: VisualEntry::Cell(cell),
                separator_after: needs_sep,
            });
            used += cost;
            cells_collected += 1;
            prev_is_user = Some(cur_is_user);
        }

        // Count message cells (User/Agent) that were clipped above.
        let skipped_message_count = self
            .committed_cells
            .iter()
            .take(self.committed_cells.len().saturating_sub(cells_collected))
            .filter(|c| matches!(c, HistoryCell::User(_) | HistoryCell::Agent(_)))
            .count();

        // Also account for a separator between the last committed cell
        // and the active cell, if the active cell was rendered above.
        let active_sep = if active_text_info.is_some() {
            // Active cell is always an agent response. If the last committed
            // cell (first in `entries` since entries is newest-first) is a
            // User cell, we need a separator.
            entries
                .first()
                .is_some_and(|e| matches!(e.visual, VisualEntry::Cell(HistoryCell::User(_))))
        } else {
            false
        };
        if active_sep && used < remaining_rows {
            used += 1;
        }

        // ── Apply scroll offset ────────────────────────────────────────
        // When the user has scrolled up, skip entries from the front
        // (newest-first). Use an index to avoid O(n²) Vec::remove(0).
        let mut skip_idx: usize = 0;
        let mut rows_to_skip = self.scroll_offset;
        while rows_to_skip > 0 && skip_idx < entries.len() {
            let entry_cost = entries[skip_idx].height as usize
                + if entries[skip_idx].separator_after {
                    1
                } else {
                    0
                };
            if entry_cost <= rows_to_skip {
                rows_to_skip -= entry_cost;
                used -= entry_cost as u16;
                skip_idx += 1;
            } else {
                break;
            }
        }
        // Remove skipped entries in one drain.
        if skip_idx > 0 {
            entries.drain(..skip_idx);
        }

        let below_count = if self.scroll_locked {
            self.scroll_offset
        } else {
            0
        };

        // ── Determine hint rows ────────────────────────────────────────
        // Reserve rows for overflow hints so they don't overwrite content.
        let dim_style = Style::default().fg(self.theme_dim);
        let show_top_hint = skipped_message_count > 0;
        let show_bottom_hint = self.scroll_locked && below_count > 0;

        // Adjust content area to leave room for hints and the active cell.
        let content_y = area.y + if show_top_hint { 1 } else { 0 };
        let content_height = area
            .height
            .saturating_sub(if show_top_hint { 1 } else { 0 })
            .saturating_sub(if show_bottom_hint { 1 } else { 0 })
            .saturating_sub(active_cell_rows);

        // Render from oldest to newest (reverse the reversed list).
        entries.reverse();
        let clamped_used = used.min(content_height);
        let mut row_cursor = content_y + content_height - clamped_used;

        let sep_style = Style::default().fg(self.theme_dim);

        for entry in &entries {
            if row_cursor >= content_y + content_height {
                break;
            }
            let avail = (content_y + content_height).saturating_sub(row_cursor);
            let h = entry.height.min(avail);
            let cell_area = Rect {
                x: area.x,
                y: row_cursor,
                width: area.width,
                height: h,
            };
            match &entry.visual {
                VisualEntry::Cell(cell) => cell.render(cell_area, buf),
                VisualEntry::GroupSummary(line) => {
                    let paragraph = Paragraph::new(vec![line.clone()]);
                    paragraph.render(cell_area, buf);
                }
            }
            row_cursor += h;

            if entry.separator_after && row_cursor < content_y + content_height {
                render_turn_separator(area.x, row_cursor, area.width, sep_style, buf);
                row_cursor += 1;
            }
        }

        // Render a separator between the last committed cell and the
        // active streaming cell, if needed.
        if active_sep && row_cursor < content_y + content_height {
            render_turn_separator(area.x, row_cursor, area.width, sep_style, buf);
        }

        // ── Overflow indicators ───────────────────────────────────────
        if show_top_hint {
            let hint = if self.scroll_locked {
                format!(
                    " \u{2191} {} more \u{00b7} PgUp to scroll",
                    skipped_message_count
                )
            } else {
                format!(
                    " \u{2191} {} more \u{00b7} PgUp to scroll, Ctrl+T for transcript",
                    skipped_message_count
                )
            };
            let hint_line = Line::from(Span::styled(hint, dim_style));
            let hint_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: 1,
            };
            Paragraph::new(hint_line).render(hint_area, buf);
        }
        if show_bottom_hint {
            let hint = " \u{2193} scrolled up \u{00b7} PgDn/End to return".to_string();
            let hint_line = Line::from(Span::styled(hint, dim_style));
            let hint_area = Rect {
                x: area.x,
                y: area.y + area.height - 1,
                width: area.width,
                height: 1,
            };
            Paragraph::new(hint_line).render(hint_area, buf);
        }
    }

    /// Render the input widget in the given row or box.
    ///
    /// `show_cursor` controls whether the input cursor is visible (used while
    /// an agent turn is running).
    pub fn render_input(&self, area: Rect, buf: &mut Buffer, is_turn_running: bool) {
        self.input.render_with_state(area, buf, !is_turn_running);
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
            cell.set_symbol("\u{2500}");
            cell.set_style(style);
        }
    }
}

use crate::history::wrapped_row_count;

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
    fn append_text_buffers_through_streaming() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Hello\n");
        cw.append_text("world\n");

        // Text is in the streaming buffer, not yet in text_buffer.
        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "");
        assert!(cw.has_streaming_pending());

        // Ticking commits one line at a time.
        assert!(cw.tick_streaming());
        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "Hello\n");

        assert!(cw.tick_streaming());
        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "Hello\nworld\n");
    }

    #[test]
    fn append_text_no_newline_stays_pending() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("partial");

        // No newline means it stays in streaming buffer pending area.
        let active = cw.active_cell.as_ref().unwrap();
        assert_eq!(active.text_buffer, "");
        // No queued lines (only partial text), so animation timer not needed.
        assert!(!cw.has_streaming_pending());
        // But the preview includes the partial text for rendering.
        assert_eq!(cw.active_cell_preview_text().unwrap(), "partial");

        // Finalize flushes the partial text (used at turn completion).
        let cells = cw.complete_turn();
        assert!(!cells.is_empty());
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
        let mut cw = ChatWidget::new();
        assert_eq!(cw.visible_content_height(80), 0);
    }

    #[test]
    fn visible_content_height_returns_zero_for_zero_width() {
        let mut cw = ChatWidget::new();
        assert_eq!(cw.visible_content_height(0), 0);
    }

    #[test]
    fn visible_content_height_counts_committed_cells() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello".to_string());
        let h = cw.visible_content_height(80);
        assert!(
            h >= 1,
            "should have at least 1 row for user message, got {h}"
        );
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
        assert_eq!(
            h, 3,
            "should include separator between user and agent, got {h}"
        );
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
                    .is_some_and(|c| c.symbol() == "\u{2500}")
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
                        .is_some_and(|c| c.symbol() == "\u{2500}")
                })
            })
            .count();
        assert_eq!(
            separator_count, 0,
            "no turn separator between consecutive agent/tool cells"
        );
    }

    #[test]
    fn overflow_indicator_shown_when_messages_clipped() {
        let mut cw = ChatWidget::new();
        // Create enough messages to overflow a small viewport.
        for i in 0..10 {
            cw.add_user_message(format!("message {i}"));
            cw.start_turn();
            cw.append_text(&format!("response {i}"));
            cw.complete_turn();
        }

        // Render into a tiny viewport (only 5 rows).
        let area = Rect::new(0, 0, 60, 5);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // The first row should contain the overflow indicator with "↑".
        let first_row: String = (0..area.width)
            .filter_map(|col| buf.cell((col, 0)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(
            first_row.contains('\u{2191}'),
            "first row should contain up-arrow indicator, got: {first_row:?}"
        );
        assert!(
            first_row.contains("more"),
            "first row should contain 'more', got: {first_row:?}"
        );
        assert!(
            first_row.contains("Ctrl+T"),
            "first row should contain 'Ctrl+T', got: {first_row:?}"
        );
    }

    #[test]
    fn no_overflow_indicator_when_all_messages_fit() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello".to_string());
        cw.start_turn();
        cw.append_text("hi");
        cw.complete_turn();

        // Render into a viewport big enough to fit everything.
        let area = Rect::new(0, 0, 60, 20);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // No row should contain the "↑" overflow indicator.
        let has_indicator = (0..area.height).any(|row| {
            let row_text: String = (0..area.width)
                .filter_map(|col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
                .collect();
            row_text.contains('\u{2191}') && row_text.contains("more")
        });
        assert!(
            !has_indicator,
            "should not show overflow indicator when all messages fit"
        );
    }

    #[test]
    fn streaming_cursor_shown_during_active_cell() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("streaming text\n");
        // Tick to commit the buffered line to the active cell.
        cw.tick_streaming();

        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // The buffer should contain the block cursor character U+258D.
        let has_cursor = (0..area.height).any(|row| {
            (0..area.width).any(|col| {
                buf.cell((col, row))
                    .is_some_and(|c| c.symbol() == "\u{258D}")
            })
        });
        assert!(has_cursor, "should render block cursor during streaming");
    }

    #[test]
    fn no_streaming_cursor_without_active_cell() {
        let mut cw = ChatWidget::new();
        cw.add_user_message("hello".to_string());
        cw.start_turn();
        cw.append_text("done");
        cw.complete_turn();

        let area = Rect::new(0, 0, 60, 10);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // No block cursor should appear once the turn is complete.
        let has_cursor = (0..area.height).any(|row| {
            (0..area.width).any(|col| {
                buf.cell((col, row))
                    .is_some_and(|c| c.symbol() == "\u{258D}")
            })
        });
        assert!(
            !has_cursor,
            "should not render block cursor after turn is complete"
        );
    }

    // ── Collapsible tool group tests ──────────────────────────────────────

    #[test]
    fn groups_start_collapsed() {
        let cw = ChatWidget::new();
        assert!(
            !cw.is_tool_group_expanded(0),
            "tool groups should start collapsed"
        );
    }

    #[test]
    fn find_tool_groups_basic() {
        let mut cw = ChatWidget::new();
        // User + Agent + 3 Tool cells (one group of 3)
        cw.add_user_message("do stuff".to_string());
        cw.start_turn();
        cw.append_text("Running tools.");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.tool_call_start("c3".into(), "grep".into(), "pat".into());
        cw.tool_call_end("c3", false, std::time::Duration::from_millis(200));
        cw.complete_turn();

        let groups = cw.find_tool_groups();
        // committed_cells: [User, Agent, Tool, Tool, Tool]
        // indices:          0     1      2     3     4
        assert_eq!(groups.len(), 1, "should find 1 group, got {groups:?}");
        assert_eq!(groups[0], (2, 3), "group should be (start=2, count=3)");
    }

    #[test]
    fn find_tool_groups_single_tool_not_grouped() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("one tool");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.complete_turn();

        let groups = cw.find_tool_groups();
        assert!(
            groups.is_empty(),
            "single tool cell should not form a group"
        );
    }

    #[test]
    fn find_tool_groups_multiple_groups() {
        let mut cw = ChatWidget::new();
        // First turn: agent + 2 tools
        cw.start_turn();
        cw.append_text("first");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        // Second turn: agent + 3 tools
        cw.start_turn();
        cw.append_text("second");
        cw.tool_call_start("c3".into(), "grep".into(), "a".into());
        cw.tool_call_end("c3", true, std::time::Duration::from_millis(30));
        cw.tool_call_start("c4".into(), "grep".into(), "b".into());
        cw.tool_call_end("c4", true, std::time::Duration::from_millis(40));
        cw.tool_call_start("c5".into(), "grep".into(), "c".into());
        cw.tool_call_end("c5", false, std::time::Duration::from_millis(50));
        cw.complete_turn();

        let groups = cw.find_tool_groups();
        // committed_cells: [Agent, Tool, Tool, Agent, Tool, Tool, Tool]
        // indices:          0      1     2     3      4     5     6
        assert_eq!(groups.len(), 2, "should find 2 groups");
        assert_eq!(groups[0], (1, 2), "first group: (1, 2)");
        assert_eq!(groups[1], (4, 3), "second group: (4, 3)");
    }

    #[test]
    fn collapsed_tool_group_renders_summary_line() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Running tools.");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        // Should have: Agent + Tool + Tool = 3 committed cells.
        assert_eq!(cw.committed_cells().len(), 3);

        // The group should be collapsed by default.
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // Scan all rows for the summary marker.
        let mut all_text = String::new();
        for row in 0..area.height {
            for col in 0..area.width {
                if let Some(c) = buf.cell((col, row)) {
                    all_text.push_str(c.symbol());
                }
            }
        }
        assert!(
            all_text.contains("2 tool calls"),
            "collapsed group should show '2 tool calls' summary, got: {all_text:?}"
        );
        assert!(
            all_text.contains("all ok"),
            "all-success group should show 'all ok', got: {all_text:?}"
        );
    }

    #[test]
    fn expanded_tool_group_renders_individual_cells() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.append_text("Running tools.");
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        // Expand the tool group (starts at index 1: Agent=0, Tool=1, Tool=2).
        cw.expand_tool_group(1);

        let area = Rect::new(0, 0, 80, 30);
        let mut buf = Buffer::empty(area);
        cw.render_messages(area, &mut buf);

        // When expanded, individual tool cells render with bordered blocks
        // containing their tool names.
        let mut all_text = String::new();
        for row in 0..area.height {
            for col in 0..area.width {
                if let Some(c) = buf.cell((col, row)) {
                    all_text.push_str(c.symbol());
                }
            }
        }
        // Should NOT show the summary line.
        assert!(
            !all_text.contains("2 tool calls"),
            "expanded group should not show summary, got: {all_text:?}"
        );
        // Should show individual tool boxes with tool names.
        assert!(
            all_text.contains("shell"),
            "expanded group should show individual tool names, got: {all_text:?}"
        );
    }

    #[test]
    fn collapsed_group_height_is_one() {
        let mut cw = ChatWidget::new();
        cw.start_turn();
        cw.tool_call_start("c1".into(), "shell".into(), "ls".into());
        cw.tool_call_end("c1", true, std::time::Duration::from_millis(100));
        cw.tool_call_start("c2".into(), "shell".into(), "pwd".into());
        cw.tool_call_end("c2", true, std::time::Duration::from_millis(50));
        cw.complete_turn();

        // With 2 tool cells (each 4 rows in Grouped mode), collapsed = 1 row.
        // Expanded = 8 rows.
        let collapsed_height = cw.visible_content_height(80);
        assert_eq!(
            collapsed_height, 1,
            "collapsed group should be 1 row, got {collapsed_height}"
        );

        cw.expand_tool_group(0);
        let expanded_height = cw.visible_content_height(80);
        assert!(
            expanded_height > collapsed_height,
            "expanded height ({expanded_height}) should be greater than collapsed ({collapsed_height})"
        );
    }
}

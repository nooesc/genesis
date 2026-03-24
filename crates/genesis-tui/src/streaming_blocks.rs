//! Block-level streaming collector for incremental cell commits.
//!
//! Receives line-by-line input from [`StreamingBuffer::tick()`] and detects
//! markdown block boundaries (paragraphs, code fences, headings) to determine
//! when a complete block should be committed as a `HistoryCell`.
//!
//! This avoids accumulating the entire LLM response in one giant cell, which
//! causes rendering issues with ratatui's word wrapping at scale.

/// Collects streaming lines and emits complete markdown blocks.
///
/// Block boundaries:
/// - **Paragraphs**: emitted on blank line or when a new block type starts
/// - **Code fences**: accumulated until the closing fence marker
/// - **Headings** (`# ...`): emitted immediately as their own block
pub struct StreamingBlockCollector {
    /// Current accumulating block text.
    buffer: String,
    /// Whether we're inside a fenced code block.
    in_code_fence: bool,
    /// The fence marker (e.g. "```" or "~~~") to match for closing.
    fence_marker: Option<String>,
    /// Number of lines accumulated in the current block.
    line_count: usize,
    /// Whether the current block contains list items or blockquotes.
    /// When true, the 20-line auto-split is disabled to avoid breaking
    /// mid-structure (list items/blockquotes lose their context in new cells).
    in_structured_block: bool,
    /// Deferred block waiting to be returned on the next `push_line` call.
    /// Used when a single `push_line` would need to emit two blocks (e.g.
    /// a paragraph followed by a heading on the same call).
    pending_emit: Option<String>,
}

impl Default for StreamingBlockCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingBlockCollector {
    /// Create a new empty collector.
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_code_fence: false,
            fence_marker: None,
            line_count: 0,
            in_structured_block: false,
            pending_emit: None,
        }
    }

    /// Push a line (including its trailing newline from StreamingBuffer).
    ///
    /// Returns `Some(block_text)` when a complete block should be committed.
    /// The returned text includes all lines of the block.
    pub fn push_line(&mut self, line: &str) -> Option<String> {
        // Return any previously deferred block first. The current line is
        // accumulated into the buffer and will be processed on the next call.
        if let Some(deferred) = self.pending_emit.take() {
            self.buffer.push_str(line);
            self.line_count += 1;
            return Some(deferred);
        }

        let trimmed = line.trim_end_matches('\n');

        // Inside a code fence — accumulate until closing marker.
        if self.in_code_fence {
            self.buffer.push_str(line);
            self.line_count += 1;
            if let Some(marker) = &self.fence_marker {
                if trimmed.trim_start().starts_with(marker.as_str())
                    && trimmed.trim().len() <= marker.len() + 1
                {
                    // Closing fence found — emit the entire code block.
                    self.in_code_fence = false;
                    self.fence_marker = None;
                    return self.emit();
                }
            }
            return None;
        }

        // Check for code fence opening.
        let stripped = trimmed.trim_start();
        if let Some(marker) = detect_code_fence(stripped) {
            // If there's accumulated text, emit it first as a paragraph.
            let prev = if !self.buffer.is_empty() {
                self.emit()
            } else {
                None
            };
            // Start accumulating the code fence.
            self.buffer.push_str(line);
            self.line_count = 1;
            self.in_code_fence = true;
            self.fence_marker = Some(marker);
            return prev;
        }

        // Check for heading (# at start of line) — emit immediately.
        if stripped.starts_with('#') && stripped.contains(' ') {
            // Emit any prior accumulated text first.
            let prev = if !self.buffer.is_empty() {
                self.emit()
            } else {
                None
            };
            // The heading is its own block — emit immediately.
            self.buffer.push_str(line);
            self.line_count = 1;
            let heading = self.emit();
            if prev.is_some() {
                // Two blocks in one call — defer the heading for next call.
                self.pending_emit = heading;
                return prev;
            }
            return heading;
        }

        // Blank line — emit accumulated paragraph.
        if trimmed.is_empty() {
            if !self.buffer.is_empty() {
                self.buffer.push_str(line);
                self.line_count += 1;
                return self.emit();
            }
            // Skip leading blank lines.
            return None;
        }

        // Detect list items and blockquotes — these are "structured" blocks
        // that should not be split mid-way by the auto-emit threshold.
        if is_list_item(stripped) || stripped.starts_with('>') {
            self.in_structured_block = true;
        }

        // Regular text line — accumulate.
        self.buffer.push_str(line);
        self.line_count += 1;

        // Auto-emit after accumulating enough lines to keep cells small.
        // Skip this when inside a structured block (list/blockquote) to
        // avoid splitting mid-structure — the block will emit on the next
        // blank line or when a different block type starts. Use a higher
        // safety limit (80 lines) to prevent unbounded growth.
        let limit = if self.in_structured_block { 80 } else { 20 };
        if self.line_count >= limit {
            return self.emit();
        }

        None
    }

    /// Flush any remaining partial block (called on turn completion).
    pub fn finalize(&mut self) -> Option<String> {
        self.in_code_fence = false;
        self.fence_marker = None;
        // If there's a pending deferred block, merge it with the buffer.
        if let Some(deferred) = self.pending_emit.take() {
            let mut combined = deferred;
            combined.push_str(&self.buffer);
            self.buffer.clear();
            self.line_count = 0;
            if combined.is_empty() {
                return None;
            }
            return Some(combined);
        }
        self.emit()
    }

    /// Return the current incomplete block text for rendering preview.
    ///
    /// Includes both the deferred pending_emit (if any) and the current
    /// buffer contents, so the display always shows all unfinished text.
    pub fn preview(&self) -> String {
        match &self.pending_emit {
            Some(deferred) => format!("{deferred}{}", self.buffer),
            None => self.buffer.clone(),
        }
    }

    /// Reset the collector, discarding any accumulated text.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.in_code_fence = false;
        self.fence_marker = None;
        self.line_count = 0;
        self.in_structured_block = false;
        self.pending_emit = None;
    }

    /// Emit the current buffer as a complete block.
    fn emit(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            return None;
        }
        self.line_count = 0;
        self.in_structured_block = false;
        Some(std::mem::take(&mut self.buffer))
    }
}

/// Detect a markdown list item prefix (e.g. `- `, `* `, `+ `, `1. `).
fn is_list_item(stripped: &str) -> bool {
    // Unordered: - , * , +  (followed by space)
    if stripped.len() >= 2 {
        let first = stripped.as_bytes()[0];
        if (first == b'-' || first == b'*' || first == b'+') && stripped.as_bytes()[1] == b' ' {
            return true;
        }
    }
    // Ordered: N. or N) followed by space (e.g. "1. ", "10) ")
    if let Some(dot_pos) = stripped.find(". ") {
        if dot_pos <= 9 && stripped[..dot_pos].bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    if let Some(paren_pos) = stripped.find(") ") {
        if paren_pos <= 9 && stripped[..paren_pos].bytes().all(|b| b.is_ascii_digit()) {
            return true;
        }
    }
    false
}

/// Detect a code fence opening marker (``` or ~~~).
/// Returns the fence marker string if found.
fn detect_code_fence(stripped: &str) -> Option<String> {
    if stripped.starts_with("```") {
        // Count backticks.
        let ticks: String = stripped.chars().take_while(|&c| c == '`').collect();
        if ticks.len() >= 3 {
            return Some(ticks);
        }
    }
    if stripped.starts_with("~~~") {
        let tildes: String = stripped.chars().take_while(|&c| c == '~').collect();
        if tildes.len() >= 3 {
            return Some(tildes);
        }
    }
    None
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_paragraph_emits_on_blank_line() {
        let mut c = StreamingBlockCollector::new();
        assert!(c.push_line("hello world\n").is_none());
        assert!(c.push_line("more text\n").is_none());
        let block = c.push_line("\n");
        assert!(block.is_some());
        assert!(block.unwrap().contains("hello world"));
    }

    #[test]
    fn code_fence_accumulates_until_close() {
        let mut c = StreamingBlockCollector::new();
        assert!(c.push_line("```rust\n").is_none());
        assert!(c.push_line("fn main() {}\n").is_none());
        let block = c.push_line("```\n");
        assert!(block.is_some());
        let text = block.unwrap();
        assert!(text.contains("```rust"));
        assert!(text.contains("fn main()"));
        assert!(text.contains("```"));
    }

    #[test]
    fn heading_emits_immediately() {
        let mut c = StreamingBlockCollector::new();
        let block = c.push_line("# Title\n");
        assert!(block.is_some());
        assert_eq!(block.unwrap(), "# Title\n");
    }

    #[test]
    fn heading_after_paragraph_emits_paragraph_first() {
        let mut c = StreamingBlockCollector::new();
        assert!(c.push_line("some text\n").is_none());
        // Heading triggers emit of the prior paragraph.
        let block = c.push_line("# Title\n");
        assert!(block.is_some());
        assert!(block.unwrap().contains("some text"));
        // The heading is buffered for the next emit.
        let heading = c.finalize();
        assert!(heading.is_some());
        assert!(heading.unwrap().contains("# Title"));
    }

    #[test]
    fn finalize_flushes_partial() {
        let mut c = StreamingBlockCollector::new();
        c.push_line("partial text\n");
        let block = c.finalize();
        assert!(block.is_some());
        assert!(block.unwrap().contains("partial"));
    }

    #[test]
    fn finalize_empty_returns_none() {
        let mut c = StreamingBlockCollector::new();
        assert!(c.finalize().is_none());
    }

    #[test]
    fn preview_shows_current_buffer() {
        let mut c = StreamingBlockCollector::new();
        c.push_line("accumulating\n");
        assert!(c.preview().contains("accumulating"));
    }

    #[test]
    fn auto_emit_after_20_lines() {
        let mut c = StreamingBlockCollector::new();
        for i in 0..19 {
            assert!(
                c.push_line(&format!("line {i}\n")).is_none(),
                "should not emit before 20 lines"
            );
        }
        let block = c.push_line("line 19\n");
        assert!(block.is_some(), "should auto-emit at 20 lines");
    }

    #[test]
    fn reset_clears_state() {
        let mut c = StreamingBlockCollector::new();
        c.push_line("```\n");
        assert!(c.in_code_fence);
        c.reset();
        assert!(!c.in_code_fence);
        assert!(c.buffer.is_empty());
        assert!(!c.in_structured_block);
    }

    #[test]
    fn nested_backticks_in_code_fence() {
        let mut c = StreamingBlockCollector::new();
        c.push_line("````\n");
        assert!(c.in_code_fence);
        // Triple backtick inside quad fence should NOT close it.
        assert!(c.push_line("```\n").is_none());
        // Quad backtick closes it.
        let block = c.push_line("````\n");
        assert!(block.is_some());
    }

    #[test]
    fn multiple_paragraphs_produce_multiple_blocks() {
        let mut c = StreamingBlockCollector::new();
        c.push_line("first paragraph\n");
        let b1 = c.push_line("\n").unwrap();
        c.push_line("second paragraph\n");
        let b2 = c.push_line("\n").unwrap();
        assert!(b1.contains("first"));
        assert!(b2.contains("second"));
    }

    #[test]
    fn tilde_fence_works() {
        let mut c = StreamingBlockCollector::new();
        assert!(c.push_line("~~~\n").is_none());
        assert!(c.push_line("code\n").is_none());
        let block = c.push_line("~~~\n");
        assert!(block.is_some());
        assert!(block.unwrap().contains("code"));
    }

    #[test]
    fn list_items_not_split_at_20_lines() {
        let mut c = StreamingBlockCollector::new();
        // Push 25 list items — should NOT auto-emit at 20.
        for i in 0..25 {
            assert!(
                c.push_line(&format!("- item {i}\n")).is_none(),
                "list should not auto-emit at line {}",
                i + 1
            );
        }
        // Blank line terminates the list block.
        let block = c.push_line("\n");
        assert!(block.is_some(), "list should emit on blank line");
        let text = block.unwrap();
        assert!(text.contains("- item 0"), "should contain first item");
        assert!(text.contains("- item 24"), "should contain last item");
    }

    #[test]
    fn blockquote_not_split_at_20_lines() {
        let mut c = StreamingBlockCollector::new();
        for i in 0..25 {
            assert!(
                c.push_line(&format!("> quote line {i}\n")).is_none(),
                "blockquote should not auto-emit at line {}",
                i + 1
            );
        }
        let block = c.push_line("\n");
        assert!(block.is_some());
        let text = block.unwrap();
        assert!(text.contains("> quote line 0"));
        assert!(text.contains("> quote line 24"));
    }

    #[test]
    fn ordered_list_not_split_at_20_lines() {
        let mut c = StreamingBlockCollector::new();
        for i in 1..=25 {
            assert!(
                c.push_line(&format!("{i}. item\n")).is_none(),
                "ordered list should not auto-emit at line {i}"
            );
        }
        let block = c.finalize();
        assert!(block.is_some());
        let text = block.unwrap();
        assert!(text.contains("1. item"));
        assert!(text.contains("25. item"));
    }

    #[test]
    fn plain_paragraph_still_splits_at_20_lines() {
        let mut c = StreamingBlockCollector::new();
        for i in 0..19 {
            assert!(
                c.push_line(&format!("plain line {i}\n")).is_none(),
                "should not emit before 20 lines"
            );
        }
        let block = c.push_line("plain line 19\n");
        assert!(block.is_some(), "plain text should auto-emit at 20 lines");
    }

    #[test]
    fn structured_block_has_safety_limit() {
        // Even structured blocks have an 80-line safety limit.
        let mut c = StreamingBlockCollector::new();
        for i in 0..79 {
            assert!(
                c.push_line(&format!("- item {i}\n")).is_none(),
                "should not emit before 80 lines for structured block"
            );
        }
        let block = c.push_line("- item 79\n");
        assert!(block.is_some(), "should emit at 80-line safety limit");
    }

    #[test]
    fn heading_followed_by_text_stays_separate() {
        // Regression test: heading after paragraph must not merge with following text.
        let mut c = StreamingBlockCollector::new();
        c.push_line("intro\n");
        let b1 = c.push_line("# Title\n"); // emits "intro\n", defers heading
        assert!(b1.is_some());
        assert!(b1.unwrap().contains("intro"), "first block should be intro");

        // The heading should be returned as a deferred block on the next push.
        let b2 = c.push_line("body text\n"); // returns deferred "# Title\n"
        assert!(b2.is_some());
        assert!(
            b2.as_ref().unwrap().contains("# Title"),
            "second block should be the heading, got: {:?}",
            b2
        );
        assert!(
            !b2.as_ref().unwrap().contains("body"),
            "heading must NOT contain body text"
        );

        // "body text\n" is now in the buffer, not yet emitted.
        let b3 = c.finalize().unwrap();
        assert!(b3.contains("body text"), "third block should be body text");
    }
}

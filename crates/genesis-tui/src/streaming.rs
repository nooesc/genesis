//! Smooth streaming animation for LLM text output.
//!
//! Buffers incoming text deltas and releases them line-by-line at a controlled
//! pace, creating a typewriter-like effect. Automatically switches to batch
//! mode when the display falls behind (e.g. fast code generation).
//!
//! Derived from Codex CLI's adaptive chunking system (MIT-licensed).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

// ── Thresholds (matching Codex's tuned values) ─────────────────────────────

/// Queue depth that triggers catch-up mode entry.
const ENTER_QUEUE_DEPTH: usize = 8;
/// Oldest queued line age that triggers catch-up mode entry.
const ENTER_OLDEST_AGE: Duration = Duration::from_millis(120);
/// Queue depth must drop to this for catch-up exit consideration.
const EXIT_QUEUE_DEPTH: usize = 2;
/// Oldest line age must drop to this for catch-up exit consideration.
const EXIT_OLDEST_AGE: Duration = Duration::from_millis(40);
/// Queue must stay below exit thresholds for this long before actually exiting.
const EXIT_HOLD: Duration = Duration::from_millis(250);
/// Cooldown after exiting catch-up that blocks re-entry.
const REENTER_HOLD: Duration = Duration::from_millis(250);
/// Severe depth that bypasses re-entry cooldown.
const SEVERE_QUEUE_DEPTH: usize = 64;
/// Severe age that bypasses re-entry cooldown.
const SEVERE_OLDEST_AGE: Duration = Duration::from_millis(300);

// ── StreamingBuffer ────────────────────────────────────────────────────────

/// A line waiting to be committed to the display.
struct QueuedLine {
    /// The line text (without trailing newline).
    text: String,
    /// When this line was enqueued.
    enqueued_at: Instant,
}

/// Buffers incoming LLM text and releases it line-by-line for smooth animation.
///
/// Text deltas are accumulated until a newline is found. Complete lines enter a
/// FIFO queue and are drained one per tick in smooth mode, or all at once in
/// catch-up mode when the display falls behind.
pub struct StreamingBuffer {
    /// Incoming text not yet terminated by a newline.
    pending: String,
    /// Lines ready to be committed to the display.
    queue: VecDeque<QueuedLine>,
    /// Adaptive chunking policy.
    policy: ChunkingPolicy,
}

impl Default for StreamingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamingBuffer {
    /// Create a new empty streaming buffer.
    pub fn new() -> Self {
        Self {
            pending: String::new(),
            queue: VecDeque::new(),
            policy: ChunkingPolicy::new(),
        }
    }

    /// Push a text delta from the LLM stream.
    ///
    /// Returns `true` if new lines were enqueued (caller should ensure the
    /// tick timer is running).
    pub fn push_delta(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }

        self.pending.push_str(text);

        // Extract all newline-terminated lines.
        let mut enqueued = false;
        let now = Instant::now();
        while let Some(nl_pos) = self.pending.find('\n') {
            let line = self.pending[..nl_pos].to_string();
            self.pending = self.pending[nl_pos + 1..].to_string();
            self.queue.push_back(QueuedLine {
                text: line,
                enqueued_at: now,
            });
            enqueued = true;
        }
        enqueued
    }

    /// Drain text to commit on this tick.
    ///
    /// In smooth mode, returns one line. In catch-up mode, returns all queued
    /// lines. Returns `None` if the queue is empty.
    pub fn tick(&mut self) -> Option<String> {
        if self.queue.is_empty() {
            return None;
        }

        let now = Instant::now();
        let snapshot = QueueSnapshot {
            depth: self.queue.len(),
            oldest_age: self
                .queue
                .front()
                .map(|q| now.duration_since(q.enqueued_at)),
        };

        let plan = self.policy.decide(&snapshot, now);
        match plan {
            DrainPlan::Single => self.queue.pop_front().map(|q| q.text + "\n"),
            DrainPlan::Batch(n) => {
                let take = n.min(self.queue.len()).max(1);
                let mut out = String::new();
                for _ in 0..take {
                    if let Some(q) = self.queue.pop_front() {
                        out.push_str(&q.text);
                        out.push('\n');
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            }
        }
    }

    /// Flush all remaining text (pending + queued) at once.
    ///
    /// Called when the LLM stream ends to ensure nothing is left in the buffer.
    pub fn finalize(&mut self) -> Option<String> {
        let mut out = String::new();
        while let Some(q) = self.queue.pop_front() {
            out.push_str(&q.text);
            out.push('\n');
        }
        if !self.pending.is_empty() {
            out.push_str(&std::mem::take(&mut self.pending));
        }
        self.policy = ChunkingPolicy::new();
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }

    /// Whether there is buffered content waiting to be committed.
    pub fn has_pending(&self) -> bool {
        !self.queue.is_empty() || !self.pending.is_empty()
    }

    /// Number of lines in the queue.
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the buffer is in catch-up mode.
    pub fn is_catching_up(&self) -> bool {
        self.policy.is_catch_up()
    }

    /// Reset the buffer, discarding all pending content.
    pub fn reset(&mut self) {
        self.pending.clear();
        self.queue.clear();
        self.policy = ChunkingPolicy::new();
    }
}

// ── ChunkingPolicy ─────────────────────────────────────────────────────────

/// Snapshot of the queue state for policy decisions.
struct QueueSnapshot {
    depth: usize,
    oldest_age: Option<Duration>,
}

/// What to drain this tick.
enum DrainPlan {
    /// Emit exactly one line (smooth mode).
    Single,
    /// Emit up to N lines (catch-up mode).
    Batch(usize),
}

/// Current animation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkingMode {
    Smooth,
    CatchUp,
}

/// Adaptive chunking policy that switches between smooth (one line per tick)
/// and catch-up (batch drain) modes based on queue depth and age.
struct ChunkingPolicy {
    mode: ChunkingMode,
    /// When the queue first dropped below exit thresholds.
    below_exit_since: Option<Instant>,
    /// When we last exited catch-up (for re-entry cooldown).
    last_exit: Option<Instant>,
}

impl ChunkingPolicy {
    fn new() -> Self {
        Self {
            mode: ChunkingMode::Smooth,
            below_exit_since: None,
            last_exit: None,
        }
    }

    fn is_catch_up(&self) -> bool {
        self.mode == ChunkingMode::CatchUp
    }

    fn decide(&mut self, snapshot: &QueueSnapshot, now: Instant) -> DrainPlan {
        if snapshot.depth == 0 {
            if self.mode == ChunkingMode::CatchUp {
                self.mode = ChunkingMode::Smooth;
                self.last_exit = Some(now);
            }
            self.below_exit_since = None;
            return DrainPlan::Single;
        }

        match self.mode {
            ChunkingMode::Smooth => {
                if self.should_enter_catch_up(snapshot, now) {
                    self.mode = ChunkingMode::CatchUp;
                    self.below_exit_since = None;
                    self.last_exit = None;
                    DrainPlan::Batch(snapshot.depth)
                } else {
                    DrainPlan::Single
                }
            }
            ChunkingMode::CatchUp => {
                self.maybe_exit_catch_up(snapshot, now);
                if self.mode == ChunkingMode::Smooth {
                    DrainPlan::Single
                } else {
                    DrainPlan::Batch(snapshot.depth)
                }
            }
        }
    }

    fn should_enter_catch_up(&self, snapshot: &QueueSnapshot, now: Instant) -> bool {
        let age = snapshot.oldest_age.unwrap_or(Duration::ZERO);
        let should_enter = snapshot.depth >= ENTER_QUEUE_DEPTH || age >= ENTER_OLDEST_AGE;
        if !should_enter {
            return false;
        }

        // Check re-entry cooldown (bypass for severe backlog).
        let is_severe = snapshot.depth >= SEVERE_QUEUE_DEPTH || age >= SEVERE_OLDEST_AGE;
        if !is_severe {
            if let Some(last) = self.last_exit {
                if now.duration_since(last) < REENTER_HOLD {
                    return false;
                }
            }
        }
        true
    }

    fn maybe_exit_catch_up(&mut self, snapshot: &QueueSnapshot, now: Instant) {
        let age = snapshot.oldest_age.unwrap_or(Duration::ZERO);
        let below_thresholds = snapshot.depth <= EXIT_QUEUE_DEPTH && age <= EXIT_OLDEST_AGE;

        if !below_thresholds {
            self.below_exit_since = None;
            return;
        }

        match self.below_exit_since {
            None => {
                self.below_exit_since = Some(now);
            }
            Some(since) => {
                if now.duration_since(since) >= EXIT_HOLD {
                    self.mode = ChunkingMode::Smooth;
                    self.last_exit = Some(now);
                    self.below_exit_since = None;
                }
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_delta_no_newline_does_not_enqueue() {
        let mut buf = StreamingBuffer::new();
        assert!(!buf.push_delta("hello"));
        assert_eq!(buf.queue_len(), 0);
        assert!(buf.has_pending());
    }

    #[test]
    fn push_delta_with_newline_enqueues() {
        let mut buf = StreamingBuffer::new();
        assert!(buf.push_delta("hello\nworld\n"));
        assert_eq!(buf.queue_len(), 2);
    }

    #[test]
    fn tick_returns_one_line_in_smooth_mode() {
        let mut buf = StreamingBuffer::new();
        buf.push_delta("line1\nline2\nline3\n");
        assert_eq!(buf.queue_len(), 3);

        let result = buf.tick().unwrap();
        assert_eq!(result, "line1\n");
        assert_eq!(buf.queue_len(), 2);
    }

    #[test]
    fn finalize_flushes_everything() {
        let mut buf = StreamingBuffer::new();
        buf.push_delta("line1\npartial");
        assert_eq!(buf.queue_len(), 1);

        let result = buf.finalize().unwrap();
        assert_eq!(result, "line1\npartial");
        assert_eq!(buf.queue_len(), 0);
        assert!(!buf.has_pending());
    }

    #[test]
    fn finalize_empty_returns_none() {
        let mut buf = StreamingBuffer::new();
        assert!(buf.finalize().is_none());
    }

    #[test]
    fn catch_up_mode_drains_batch() {
        let mut buf = StreamingBuffer::new();
        // Enqueue many lines to trigger catch-up.
        let mut text = String::new();
        for i in 0..10 {
            text.push_str(&format!("line{i}\n"));
        }
        buf.push_delta(&text);

        // Simulate time passing so oldest_age exceeds threshold.
        // Directly manipulate enqueued_at for testing.
        let old_time = Instant::now() - Duration::from_millis(200);
        for q in &mut buf.queue {
            q.enqueued_at = old_time;
        }

        let result = buf.tick().unwrap();
        // Should drain all 10 lines in one batch (catch-up mode).
        let line_count = result.matches('\n').count();
        assert_eq!(line_count, 10);
        assert_eq!(buf.queue_len(), 0);
    }

    #[test]
    fn reset_clears_everything() {
        let mut buf = StreamingBuffer::new();
        buf.push_delta("hello\nworld\npartial");
        assert!(buf.has_pending());
        buf.reset();
        assert!(!buf.has_pending());
        assert_eq!(buf.queue_len(), 0);
    }

    #[test]
    fn policy_starts_smooth() {
        let policy = ChunkingPolicy::new();
        assert_eq!(policy.mode, ChunkingMode::Smooth);
    }

    #[test]
    fn policy_enters_catch_up_on_depth() {
        let mut policy = ChunkingPolicy::new();
        let snapshot = QueueSnapshot {
            depth: ENTER_QUEUE_DEPTH,
            oldest_age: Some(Duration::ZERO),
        };
        let plan = policy.decide(&snapshot, Instant::now());
        assert!(policy.is_catch_up());
        assert!(matches!(plan, DrainPlan::Batch(_)));
    }

    #[test]
    fn policy_enters_catch_up_on_age() {
        let mut policy = ChunkingPolicy::new();
        let snapshot = QueueSnapshot {
            depth: 1,
            oldest_age: Some(ENTER_OLDEST_AGE),
        };
        let plan = policy.decide(&snapshot, Instant::now());
        assert!(policy.is_catch_up());
        assert!(matches!(plan, DrainPlan::Batch(_)));
    }

    #[test]
    fn policy_stays_smooth_below_thresholds() {
        let mut policy = ChunkingPolicy::new();
        let snapshot = QueueSnapshot {
            depth: 3,
            oldest_age: Some(Duration::from_millis(50)),
        };
        let plan = policy.decide(&snapshot, Instant::now());
        assert!(!policy.is_catch_up());
        assert!(matches!(plan, DrainPlan::Single));
    }
}

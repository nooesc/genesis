//! Frame draw scheduling utilities for the TUI.
//!
//! This module exposes [`FrameRequester`], a lightweight handle that widgets and
//! background tasks can clone to request future redraws of the TUI.
//!
//! Internally it spawns a background task that coalesces many requests into a
//! single notification on a broadcast channel used by the main TUI event loop.
//! This keeps animations and status updates smooth without redrawing more often
//! than necessary.
//!
//! This follows the actor-style design from
//! ["Actors with Tokio"](https://ryhl.io/blog/actors-with-tokio/), with a
//! dedicated scheduler task and lightweight request handles.

use std::time::Duration;
use std::time::Instant;

use tokio::sync::broadcast;
use tokio::sync::mpsc;

/// Maximum draw rate: ~120 FPS.
const MIN_FRAME_INTERVAL: Duration = Duration::from_millis(8);

/// Handle for requesting frame redraws. Clone-cheap.
///
/// Clones of this type can be freely shared across tasks to trigger frame draws
/// from anywhere in the TUI code.
#[derive(Clone, Debug)]
pub struct FrameRequester {
    tx: mpsc::UnboundedSender<Instant>,
}

impl FrameRequester {
    /// Create a new `FrameRequester` and spawn its associated scheduler task.
    ///
    /// The provided `draw_tx` is used to notify the TUI event loop of scheduled draws.
    pub fn new(draw_tx: broadcast::Sender<()>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(frame_scheduler(rx, draw_tx));
        Self { tx }
    }

    /// Request a frame redraw as soon as possible (rate-limited to ~120 FPS).
    pub fn schedule_frame(&self) {
        let _ = self.tx.send(Instant::now());
    }

    /// Request a frame redraw after the specified delay.
    pub fn schedule_frame_in(&self, delay: Duration) {
        let _ = self.tx.send(Instant::now() + delay);
    }
}

/// Background actor that coalesces draw requests and emits rate-limited signals.
///
/// Runs until all [`FrameRequester`] handles are dropped (channel closes).
async fn frame_scheduler(mut rx: mpsc::UnboundedReceiver<Instant>, draw_tx: broadcast::Sender<()>) {
    let mut last_emitted: Option<Instant> = None;
    let mut next_deadline: Option<Instant> = None;

    loop {
        // If no pending deadline, sleep for a very long time until a request arrives.
        let sleep_until =
            next_deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));

        tokio::select! {
            request = rx.recv() => {
                let Some(requested_at) = request else {
                    // All senders dropped; exit.
                    break;
                };
                // Clamp the requested instant to respect the minimum frame interval.
                let min_allowed = last_emitted
                    .map(|t| t + MIN_FRAME_INTERVAL)
                    .unwrap_or(requested_at);
                let clamped = requested_at.max(min_allowed);
                // Coalesce: keep the earliest pending deadline.
                next_deadline = Some(
                    next_deadline.map_or(clamped, |cur: Instant| cur.min(clamped)),
                );
            }
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(sleep_until)) => {
                if next_deadline.is_some() {
                    next_deadline = None;
                    last_emitted = Some(Instant::now());
                    let _ = draw_tx.send(());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast;

    #[tokio::test]
    async fn schedule_frame_sends_draw_signal() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame();
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(draw_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn multiple_requests_coalesce() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        // Send 10 requests rapidly
        for _ in 0..10 {
            requester.schedule_frame();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Should receive 1-2 signals, not 10
        let mut count = 0;
        while draw_rx.try_recv().is_ok() {
            count += 1;
        }
        assert!(count <= 2, "got {count} signals, expected <= 2");
    }

    #[tokio::test]
    async fn schedule_frame_in_delays() {
        let (draw_tx, mut draw_rx) = broadcast::channel(16);
        let requester = FrameRequester::new(draw_tx);
        requester.schedule_frame_in(Duration::from_millis(50));
        // Should not have fired yet
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(draw_rx.try_recv().is_err());
        // Should fire after delay
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(draw_rx.try_recv().is_ok());
    }
}

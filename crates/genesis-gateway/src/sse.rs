//! SSE helper functions for streaming responses.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::response::sse::Event;
use tokio::sync::mpsc;
use tracing::{debug, error};

/// Buffer size for bounded SSE channels.  Provides backpressure: when the
/// buffer fills up, the sender blocks (up to [`SSE_SEND_TIMEOUT`]) rather
/// than silently dropping events.
pub(crate) const SSE_CHANNEL_BUFFER: usize = 64;

/// Default timeout for SSE streaming requests (5 minutes).  Overridable via
/// `gateway.stream_timeout_secs` in the config file.
pub(crate) const DEFAULT_STREAM_TIMEOUT_SECS: u64 = 300;

/// Maximum time to wait when the SSE channel buffer is full before aborting
/// the stream.  This prevents silent data loss: instead of dropping events
/// when a slow consumer falls behind, we apply backpressure and only abort
/// (with cancellation) if the consumer cannot keep up for this duration.
pub(crate) const SSE_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Send an SSE event on a bounded channel with backpressure.
///
/// If the receiver has been dropped (client disconnected) the cancellation
/// flag is set so the agent loop exits at the next opportunity.  If the
/// channel buffer is full, this blocks for up to [`SSE_SEND_TIMEOUT`]
/// waiting for capacity.  If the timeout elapses the stream is aborted via
/// cancellation — this is preferable to silently dropping events which would
/// cause invisible data loss in OpenAI-compatible streaming responses.
///
/// This function is called from synchronous streaming callbacks, so it uses
/// [`tokio::task::block_in_place`] to safely block the current thread while
/// awaiting the async send.
pub(crate) fn send_sse(
    tx: &mpsc::Sender<Result<Event, std::convert::Infallible>>,
    event: Result<Event, std::convert::Infallible>,
    cancelled: &AtomicBool,
) {
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    // block_in_place moves the current task off the tokio worker thread,
    // allowing us to block on an async send without deadlocking the runtime.
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            match tokio::time::timeout(SSE_SEND_TIMEOUT, tx.send(event)).await {
                Ok(Ok(())) => {}
                Ok(Err(_closed)) => {
                    debug!("SSE client disconnected, signalling cancellation");
                    cancelled.store(true, Ordering::Relaxed);
                }
                Err(_elapsed) => {
                    error!(
                        timeout_secs = SSE_SEND_TIMEOUT.as_secs(),
                        "SSE send timed out (slow consumer), aborting stream"
                    );
                    cancelled.store(true, Ordering::Relaxed);
                }
            }
        });
    });
}

/// Guard that sets a cancellation flag on drop.  Used to ensure the agent
/// loop is cancelled when the SSE stream is dropped — whether that happens
/// because the stream naturally ended or because Axum dropped the future
/// mid-execution (client disconnect).
pub(crate) struct CancelOnDrop(pub(crate) Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

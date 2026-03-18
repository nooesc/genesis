//! TUI approval handler — bridges blocking tool approval with async TUI event loop.
//!
//! When a tool requires approval, this handler:
//! 1. Sends an `ApprovalRequest` to the TUI event loop via channel
//! 2. Blocks the tool execution thread waiting for the user's response
//! 3. Returns true (approved) or false (denied)
//!
//! The TUI shows an overlay with the tool name and arguments, and the
//! user presses `y` to approve or `n`/`Esc` to deny.

use std::collections::BTreeMap;

/// A pending approval request sent from the tool execution thread to the TUI.
pub struct ApprovalRequest {
    /// Name of the tool requesting approval.
    pub tool_name: String,
    /// Tool arguments (keys → values).
    pub arguments: BTreeMap<String, String>,
    /// Channel to send the response back on (true = approved).
    pub response_tx: std::sync::mpsc::Sender<bool>,
}

/// TUI-based approval handler. Sends requests to the TUI event loop
/// and blocks until the user responds.
pub struct TuiApprovalHandler {
    /// Channel to send approval requests to the TUI.
    request_tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>,
}

impl TuiApprovalHandler {
    pub fn new(request_tx: tokio::sync::mpsc::UnboundedSender<ApprovalRequest>) -> Self {
        Self { request_tx }
    }
}

impl genesis_tools::ApprovalHandler for TuiApprovalHandler {
    fn request_approval(
        &self,
        tool_name: &str,
        arguments: &BTreeMap<String, String>,
    ) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        let request = ApprovalRequest {
            tool_name: tool_name.to_owned(),
            arguments: arguments.clone(),
            response_tx: tx,
        };

        // Send to TUI event loop. If the channel is closed (TUI exited),
        // deny the request.
        if self.request_tx.send(request).is_err() {
            tracing::warn!(tool_name, "approval channel closed, denying request");
            return false;
        }

        // Block until the user responds. The TUI event loop will send
        // true/false on the oneshot channel when the user presses y/n.
        match rx.recv() {
            Ok(approved) => {
                tracing::info!(tool_name, approved, "tool approval response");
                approved
            }
            Err(_) => {
                tracing::warn!(tool_name, "approval response channel dropped, denying");
                false
            }
        }
    }
}

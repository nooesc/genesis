//! Genesis TUI — ratatui-based terminal interface for Eve.
//!
//! Ported from Codex CLI's inline-viewport architecture. The TUI renders
//! in an inline viewport at the bottom of the terminal; completed
//! conversation turns are pushed into terminal scrollback via DECSTBM
//! scroll regions.

pub mod custom_terminal;
pub mod frame_requester;
pub mod insert_history;
pub mod terminal;

use genesis_config::GenesisConfig;
use genesis_core::execution::SessionExecutionService;

/// Entry point for the ratatui TUI.
///
/// Called from `genesis chat --tui` (the default).
pub async fn run_tui(
    _config: &GenesisConfig,
    _service: &SessionExecutionService<'_>,
    _session_id: &str,
) -> Result<(), TuiError> {
    todo!("TUI implementation")
}

/// Errors that can occur in the TUI.
#[derive(Debug, thiserror::Error)]
pub enum TuiError {
    #[error("terminal error: {0}")]
    Terminal(#[from] std::io::Error),

    #[error("agent error: {0}")]
    Agent(String),
}

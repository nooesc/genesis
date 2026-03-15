//! Terminal lifecycle management — raw mode, keyboard enhancement, panic hook.
//!
//! Ported from Codex's `tui.rs`. Manages the crossterm terminal state and
//! ensures cleanup on exit or panic.

use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste,
        EnableFocusChange, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io::{self, stdout, IsTerminal};
use std::panic;

/// Enable raw mode, keyboard enhancements, and bracketed paste.
pub fn init() -> io::Result<()> {
    if !stdout().is_terminal() {
        return Err(io::Error::other("stdout is not a terminal"));
    }

    enable_raw_mode()?;
    execute!(stdout(), EnableBracketedPaste)?;

    // Best-effort keyboard enhancement (not supported on all terminals)
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        )
    );

    let _ = execute!(stdout(), EnableFocusChange);

    // Flush any buffered input from before raw mode
    flush_stdin();

    set_panic_hook();

    Ok(())
}

/// Restore terminal to normal state. Safe to call multiple times.
pub fn restore() -> io::Result<()> {
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(stdout(), DisableBracketedPaste);
    let _ = execute!(stdout(), DisableFocusChange);
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show);
    Ok(())
}

/// Set panic hook that restores terminal before printing panic info.
fn set_panic_hook() {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        let _ = restore();
        original_hook(panic_info);
    }));
}

/// Flush any buffered stdin bytes (prevents stale input after mode switch).
fn flush_stdin() {
    #[cfg(unix)]
    unsafe {
        libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
}

/// Check if running inside Zellij (strict xterm compliance, no alt screen).
pub fn is_zellij() -> bool {
    std::env::var("ZELLIJ_SESSION_NAME").is_ok()
}

/// Check if running inside tmux.
pub fn is_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_is_idempotent() {
        // Calling restore twice should not panic
        let _ = restore();
        let _ = restore();
    }

    #[test]
    fn detect_zellij_from_env() {
        std::env::remove_var("ZELLIJ_SESSION_NAME");
        assert!(!is_zellij());
    }

    #[test]
    fn detect_tmux_from_env() {
        std::env::remove_var("TMUX");
        assert!(!is_tmux());
    }
}

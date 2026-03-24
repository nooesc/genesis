//! Terminal lifecycle management — raw mode, keyboard enhancement, panic hook.
//!
//! Ported from Codex's `tui.rs`. Manages the crossterm terminal state and
//! ensures cleanup on exit or panic.

use crossterm::{
    cursor,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use std::io::{self, stdout, IsTerminal, Write};
use std::panic;
use std::sync::Once;

/// Whether alternate screen mode should be used.
///
/// Zellij does not support alternate screen properly (users lose scrollback),
/// so it is disabled automatically when running inside Zellij. Users can also
/// pass `--no-alt-screen` to force inline mode in any terminal.
static ALT_SCREEN_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Check whether alternate screen is enabled for this session.
pub fn is_alt_screen_enabled() -> bool {
    ALT_SCREEN_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Enable raw mode, keyboard enhancements, and bracketed paste.
///
/// Enters alternate screen and hides the cursor for a full-screen TUI.
/// If `alt_screen` is `false` (or Zellij is detected), alternate screen
/// is skipped to preserve terminal scrollback.
pub fn init_with_options(alt_screen: bool) -> io::Result<()> {
    if !stdout().is_terminal() {
        return Err(io::Error::other("stdout is not a terminal"));
    }

    // Auto-disable alt screen in Zellij (it doesn't support it properly).
    let use_alt_screen = alt_screen && !is_zellij();
    ALT_SCREEN_ENABLED.store(use_alt_screen, std::sync::atomic::Ordering::Relaxed);

    enable_raw_mode()?;
    let _ = execute!(stdout(), cursor::Hide);
    if use_alt_screen {
        let _ = execute!(stdout(), EnterAlternateScreen);
    }
    execute!(stdout(), EnableBracketedPaste)?;

    // Best-effort keyboard enhancement (not supported on all terminals).
    // DISAMBIGUATE_ESCAPE_CODES enables Shift+Enter detection in Kitty/WezTerm.
    // REPORT_ALL_KEYS_AS_ESCAPE_CODES improves modifier detection for Enter/Tab.
    let _ = execute!(
        stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    );

    let _ = execute!(stdout(), EnableFocusChange);
    let _ = execute!(stdout(), EnableMouseCapture);

    // Enable alternate scroll mode (DEC private mode 1007).
    // Translates mouse wheel events into up/down arrow key sequences in
    // alternate screen mode — required for tmux to forward scroll events.
    let _ = stdout().write_all(b"\x1b[?1007h");
    let _ = stdout().flush();

    // Flush any buffered input from before raw mode
    flush_stdin();

    set_panic_hook();

    Ok(())
}

/// Enable raw mode with alternate screen (backward-compatible default).
pub fn init() -> io::Result<()> {
    init_with_options(true)
}

/// Restore terminal to normal state. Safe to call multiple times.
///
/// Only leaves alternate screen if it was entered during `init`.
pub fn restore() -> io::Result<()> {
    let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(stdout(), DisableMouseCapture);
    // Disable alternate scroll mode (DEC private mode 1007).
    let _ = stdout().write_all(b"\x1b[?1007l");
    let _ = stdout().flush();
    let _ = execute!(stdout(), DisableBracketedPaste);
    let _ = execute!(stdout(), DisableFocusChange);
    if is_alt_screen_enabled() {
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show);
    Ok(())
}

/// Guard ensuring the panic hook is installed at most once.
static PANIC_HOOK_ONCE: Once = Once::new();

/// Set panic hook that restores terminal before printing panic info.
///
/// Guarded by `Once` so that repeated `init()` calls (e.g. after
/// suspend/resume cycles) do not nest hooks, which would cause
/// `restore()` to be invoked N+1 times on a single panic.
fn set_panic_hook() {
    PANIC_HOOK_ONCE.call_once(|| {
        let original_hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            let _ = restore();
            original_hook(panic_info);
        }));
    });
}

/// Flush any buffered stdin bytes (prevents stale input after mode switch).
fn flush_stdin() {
    #[cfg(unix)]
    // SAFETY: STDIN_FILENO is always a valid fd. If stdin has been redirected
    // to a non-tty, tcflush returns ENOTTY which we silently ignore (the
    // return value is discarded). This is only called after init() confirms
    // stdout is a terminal, at which point stdin is also expected to be a tty.
    unsafe {
        libc::tcflush(libc::STDIN_FILENO, libc::TCIFLUSH);
    }
}

// Re-export terminal detection from genesis-ui to avoid duplication.
pub use genesis_ui::terminal::{is_tmux, is_zellij};

/// Returns `true` if we are running inside a terminal multiplexer (tmux or Zellij).
///
/// Multiplexers handle Ctrl+Z / SIGTSTP themselves, so sending `kill(0, SIGTSTP)`
/// from the application can cause unexpected behavior (double-suspend, detach, etc.).
pub fn is_multiplexer() -> bool {
    is_tmux() || is_zellij()
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

    #[test]
    fn is_multiplexer_false_outside_mux() {
        std::env::remove_var("TMUX");
        std::env::remove_var("ZELLIJ_SESSION_NAME");
        assert!(!is_multiplexer());
    }
}

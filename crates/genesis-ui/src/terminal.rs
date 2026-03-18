//! Terminal detection and helpers — size queries, tmux detection, color support.

use std::io::IsTerminal;

use crossterm::terminal;

/// Terminal dimensions.
#[derive(Debug, Clone, Copy)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

impl TerminalSize {
    /// Query the current terminal size. Falls back to 80x24 on failure.
    pub fn detect() -> Self {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        Self { cols, rows }
    }
}

/// Whether color output should be enabled, based on mode and environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    /// Auto-detect: enabled if stdout is a terminal and `NO_COLOR` is not set.
    Auto,
    /// Always emit ANSI color codes.
    Always,
    /// Never emit ANSI color codes.
    Never,
}

impl ColorMode {
    /// Resolve whether colors are actually enabled for this mode.
    pub fn is_enabled(self) -> bool {
        match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                if std::env::var_os("NO_COLOR").is_some() {
                    return false;
                }
                std::io::stdout().is_terminal()
            }
        }
    }
}

/// Returns `true` if we are running inside tmux.
pub fn is_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Returns `true` if we are running inside Zellij.
///
/// Zellij has strict xterm spec compliance — alternate screen and some
/// DECSTBM features behave differently than in other multiplexers.
pub fn is_zellij() -> bool {
    std::env::var_os("ZELLIJ_SESSION_NAME").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_size_detect_returns_reasonable_values() {
        let size = TerminalSize::detect();
        assert!(size.cols > 0);
        assert!(size.rows > 0);
    }

    #[test]
    fn color_mode_always_is_enabled() {
        assert!(ColorMode::Always.is_enabled());
    }

    #[test]
    fn color_mode_never_is_disabled() {
        assert!(!ColorMode::Never.is_enabled());
    }
}

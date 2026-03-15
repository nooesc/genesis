//! Row-by-row reveal animation for the Eve banner.
//!
//! Instead of frame-cycling, the half-block art is revealed one row at a
//! time from top to bottom with a slight delay, creating a "materializing"
//! effect. The animation takes ~0.3-0.5s depending on art height.

use std::io::{self, IsTerminal, Write};
use std::thread;
use std::time::Duration;

/// Delay between each row during the reveal animation.
const ROW_DELAY_MS: u64 = 12;

/// Reveal the art row by row with a slight delay per row.
///
/// If stdout is not a terminal, prints all lines immediately (no animation).
pub fn reveal_animation(lines: &[String]) {
    if lines.is_empty() {
        return;
    }

    let mut stdout = io::stdout();
    let is_tty = stdout.is_terminal();

    if !is_tty {
        // Not a terminal — just print everything at once.
        for line in lines {
            println!("{line}");
        }
        return;
    }

    // Reveal row by row
    for line in lines {
        println!("{line}");
        let _ = stdout.flush();
        thread::sleep(Duration::from_millis(ROW_DELAY_MS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_animation_does_not_panic_with_empty() {
        reveal_animation(&[]);
    }

    #[test]
    fn reveal_animation_does_not_panic_with_lines() {
        let lines = vec!["test line 1".to_string(), "test line 2".to_string()];
        reveal_animation(&lines);
    }
}

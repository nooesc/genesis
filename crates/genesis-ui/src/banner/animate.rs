//! Frame cycling and cursor control for the Eve banner animation.
//!
//! Uses crossterm cursor positioning to redraw frames in-place without
//! an alternate screen buffer — the animation happens within the normal
//! scrollback.

use std::io::{self, IsTerminal, Write};
use std::thread;
use std::time::Duration;

use crossterm::{cursor, execute, terminal};

/// The animation frame sequence. We cycle: 1, 2, 1, 3, 1, 2, 1, 3
/// to create a natural sway that returns through neutral.
const FRAME_SEQUENCE: [usize; 8] = [0, 1, 0, 2, 0, 1, 0, 2];

/// Duration per frame in milliseconds.
const FRAME_DURATION_MS: u64 = 180;

/// Play the sway animation by cycling through frames in-place.
///
/// This prints the first frame, then uses cursor repositioning to overwrite
/// it with subsequent frames. After the animation completes, the final
/// (neutral) frame remains on screen.
///
/// If stdout is not a terminal, the animation is skipped and only the
/// final frame is printed.
pub fn play_animation(frames: &[Vec<String>]) {
    if frames.is_empty() {
        return;
    }

    let mut stdout = io::stdout();
    let is_tty = io::stdout().is_terminal();

    if !is_tty || frames.len() < 3 {
        // Not a terminal or not enough frames — just print the resting frame.
        for line in &frames[0] {
            println!("{line}");
        }
        return;
    }

    let frame_height = frames[0].len();

    // Print the first frame to establish the region.
    for line in &frames[0] {
        println!("{line}");
    }
    let _ = stdout.flush();

    // Animate through the sequence.
    for &frame_idx in &FRAME_SEQUENCE {
        thread::sleep(Duration::from_millis(FRAME_DURATION_MS));

        let frame = &frames[frame_idx.min(frames.len() - 1)];

        // Move cursor up to overwrite the frame region.
        let _ = execute!(stdout, cursor::MoveUp(frame_height as u16));

        for line in frame {
            // Clear the current line and print the new frame line.
            let _ = execute!(stdout, terminal::Clear(terminal::ClearType::CurrentLine));
            println!("{line}");
        }
        let _ = stdout.flush();
    }

    // Ensure we end on the neutral frame (index 0).
    thread::sleep(Duration::from_millis(FRAME_DURATION_MS));
    let _ = execute!(stdout, cursor::MoveUp(frame_height as u16));
    for line in &frames[0] {
        let _ = execute!(stdout, terminal::Clear(terminal::ClearType::CurrentLine));
        println!("{line}");
    }
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_animation_does_not_panic_with_empty_frames() {
        play_animation(&[]);
    }

    #[test]
    fn play_animation_does_not_panic_with_single_frame() {
        let frames = vec![vec!["test line".to_string()]];
        // In test (non-TTY), this just prints without animation.
        play_animation(&frames);
    }

    #[test]
    fn frame_sequence_indices_valid() {
        for &idx in &FRAME_SEQUENCE {
            assert!(idx < 4, "frame sequence index {idx} exceeds frame count");
        }
    }
}

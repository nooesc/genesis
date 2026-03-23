//! Half-block pixel art types for the Eve banner.
//!
//! Provides [`HalfBlockFrame`], [`HalfBlockCell`], and [`RgbColor`] types used
//! across CLI and TUI rendering paths. No portrait art is currently displayed —
//! the welcome screen shows the "GENESIS" title and session info.

/// Number of welcome animation frames (kept for API compatibility).
pub const WELCOME_FRAME_COUNT: usize = 3;

/// A simple RGB color used by half-block cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RgbColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl RgbColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn from_tuple(rgb: (u8, u8, u8)) -> Self {
        Self::new(rgb.0, rgb.1, rgb.2)
    }
}

/// A single terminal half-block cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HalfBlockCell {
    pub symbol: char,
    pub fg: Option<RgbColor>,
    pub bg: Option<RgbColor>,
}

/// A rendered half-block frame sized in terminal character cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalfBlockFrame {
    pub width: u16,
    pub height: u16,
    pub lines: Vec<Vec<HalfBlockCell>>,
}

/// Render the full-size banner art as ANSI-escaped terminal strings.
///
/// Returns an empty vec — the welcome screen shows the "GENESIS" title
/// and session info without portrait art.
pub fn full_art() -> Vec<String> {
    Vec::new()
}

/// Render the compact banner art as ANSI-escaped terminal strings.
///
/// Returns an empty vec — no portrait art is displayed.
pub fn compact_art() -> Vec<String> {
    Vec::new()
}

/// Render all welcome animation frames to a target terminal size.
///
/// Returns empty frames — no portrait art is displayed.
pub fn render_welcome_frames(target_width: u16, target_height: u16) -> Vec<HalfBlockFrame> {
    (0..WELCOME_FRAME_COUNT)
        .map(|_| empty_frame(target_width, target_height))
        .collect()
}

/// Render one welcome animation frame to a target terminal size.
pub fn render_welcome_frame(
    _index: usize,
    target_width: u16,
    target_height: u16,
) -> HalfBlockFrame {
    empty_frame(target_width, target_height)
}

fn empty_frame(width: u16, height: u16) -> HalfBlockFrame {
    HalfBlockFrame {
        width,
        height,
        lines: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_art_returns_empty() {
        assert!(full_art().is_empty());
    }

    #[test]
    fn compact_art_returns_empty() {
        assert!(compact_art().is_empty());
    }

    #[test]
    fn render_welcome_frames_produces_correct_count() {
        let frames = render_welcome_frames(24, 12);
        assert_eq!(frames.len(), WELCOME_FRAME_COUNT);
    }
}

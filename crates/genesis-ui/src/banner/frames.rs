//! Half-block pixel art types and procedural frame generation for the Eve banner.
//!
//! Provides [`HalfBlockFrame`], [`HalfBlockCell`], and [`RgbColor`] types used
//! across CLI and TUI rendering paths. Frame content is generated procedurally
//! by the [`super::procedural`] module — no image assets are embedded.

use super::procedural;

/// Number of bundled welcome animation frames.
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

/// Generate a procedural frame at the given terminal cell dimensions.
///
/// `t` is the animation parameter in `[0.0, 1.0]`.
pub fn generate_frame(width: u16, height: u16, t: f64) -> HalfBlockFrame {
    procedural::compose(width, height, t)
}

/// Render the full-size Eve banner art as ANSI-escaped terminal strings.
pub fn full_art() -> Vec<String> {
    frame_to_ansi_lines(&generate_frame(40, 35, 0.0))
}

/// Render the compact Eve banner art as ANSI-escaped terminal strings.
pub fn compact_art() -> Vec<String> {
    frame_to_ansi_lines(&generate_frame(20, 18, 0.0))
}

/// Render all welcome animation frames to a target terminal size.
pub fn render_welcome_frames(target_width: u16, target_height: u16) -> Vec<HalfBlockFrame> {
    (0..WELCOME_FRAME_COUNT)
        .map(|index| render_welcome_frame(index, target_width, target_height))
        .collect()
}

/// Render one welcome animation frame to a target terminal size.
///
/// Maps the frame index to an animation parameter `t` and generates
/// the frame procedurally.
pub fn render_welcome_frame(
    index: usize,
    target_width: u16,
    target_height: u16,
) -> HalfBlockFrame {
    let t = index as f64 / WELCOME_FRAME_COUNT as f64;
    generate_frame(target_width, target_height, t)
}

/// Convert a [`HalfBlockFrame`] to ANSI-escaped terminal strings.
pub(crate) fn frame_to_ansi_lines(frame: &HalfBlockFrame) -> Vec<String> {
    frame
        .lines
        .iter()
        .map(|row| {
            let mut line = String::with_capacity(row.len() * 24);
            let mut active_style = false;

            for cell in row {
                match cell.symbol {
                    ' ' => {
                        if active_style {
                            line.push_str("\x1b[0m");
                            active_style = false;
                        }
                        line.push(' ');
                    }
                    symbol => {
                        line.push_str("\x1b[0m");
                        if let Some(fg) = cell.fg {
                            line.push_str(&format!("\x1b[38;2;{};{};{}m", fg.r, fg.g, fg.b));
                        }
                        if let Some(bg) = cell.bg {
                            line.push_str(&format!("\x1b[48;2;{};{};{}m", bg.r, bg.g, bg.b));
                        }
                        line.push(symbol);
                        active_style = true;
                    }
                }
            }

            line.push_str("\x1b[0m");
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_art_loads_without_panic() {
        let lines = full_art();
        assert!(!lines.is_empty());
    }

    #[test]
    fn compact_art_loads_without_panic() {
        let lines = compact_art();
        assert!(!lines.is_empty());
    }

    #[test]
    fn generate_frame_respects_target_size() {
        let frame = generate_frame(24, 12, 0.0);
        assert_eq!(frame.width, 24);
        assert_eq!(frame.height, 12);
        assert_eq!(frame.lines.len(), 12);
        assert!(frame.lines.iter().all(|line| line.len() == 24));
    }

    #[test]
    fn generate_frame_preserves_color_detail() {
        let frame = generate_frame(40, 35, 0.0);
        let unique_colors = frame
            .lines
            .iter()
            .flatten()
            .flat_map(|cell| [cell.fg, cell.bg])
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();

        assert!(unique_colors.len() > 8);
    }

    #[test]
    fn render_welcome_frames_produces_correct_count() {
        let frames = render_welcome_frames(24, 12);
        assert_eq!(frames.len(), WELCOME_FRAME_COUNT);
    }

    #[test]
    fn generate_frame_zero_size() {
        let frame = generate_frame(0, 0, 0.0);
        assert_eq!(frame.width, 0);
        assert_eq!(frame.height, 0);
        assert!(frame.lines.is_empty());
    }

    #[test]
    fn animation_frames_differ() {
        let frame_a = generate_frame(40, 35, 0.0);
        let frame_b = generate_frame(40, 35, 0.25);
        assert_ne!(frame_a, frame_b, "frames at different t values should differ");
    }
}

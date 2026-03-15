//! Half-block pixel art rendering for the Eve banner.
//!
//! Converts an embedded PNG image to terminal art using Unicode half-block
//! characters (▀▄) with truecolor ANSI escape codes. Each character cell
//! represents 2 vertical pixels (top pixel = foreground, bottom pixel =
//! background), giving effectively double vertical resolution.
//!
//! This is the same technique used by terminal image viewers and the
//! cli-music album art renderer.

use image::{DynamicImage, GenericImageView};

/// Embedded Eve art (full size).
const EVE_FULL_PNG: &[u8] = include_bytes!("eve_full.png");

/// Embedded Eve art (compact size).
const EVE_COMPACT_PNG: &[u8] = include_bytes!("eve_compact.png");

/// Alpha threshold below which a pixel is considered transparent.
const ALPHA_THRESHOLD: u8 = 30;

/// Render an RGBA image to half-block terminal art lines.
///
/// Each output line represents 2 rows of pixels. For each column:
/// - If both pixels are transparent: space
/// - If top opaque, bottom transparent: ▀ with fg = top color
/// - If top transparent, bottom opaque: ▄ with fg = bottom color
/// - If both opaque: ▀ with fg = top color, bg = bottom color
pub fn render_halfblocks(img: &DynamicImage) -> Vec<String> {
    let (width, height) = img.dimensions();
    let mut lines = Vec::new();

    let mut y = 0u32;
    while y < height {
        let mut line = String::with_capacity(width as usize * 30);
        let mut last_was_colored = false;

        for x in 0..width {
            let top = img.get_pixel(x, y);
            let bot = if y + 1 < height {
                Some(img.get_pixel(x, y + 1))
            } else {
                None
            };

            let top_visible = top[3] > ALPHA_THRESHOLD;
            let bot_visible = bot.is_some_and(|p| p[3] > ALPHA_THRESHOLD);

            match (top_visible, bot_visible) {
                (true, true) => {
                    let b = bot.unwrap();
                    line.push_str(&format!(
                        "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}m▀",
                        top[0], top[1], top[2], b[0], b[1], b[2],
                    ));
                    last_was_colored = true;
                }
                (true, false) => {
                    if last_was_colored {
                        line.push_str("\x1b[0m");
                    }
                    line.push_str(&format!(
                        "\x1b[38;2;{};{};{}m▀",
                        top[0], top[1], top[2],
                    ));
                    last_was_colored = true;
                }
                (false, true) => {
                    if last_was_colored {
                        line.push_str("\x1b[0m");
                    }
                    let b = bot.unwrap();
                    line.push_str(&format!(
                        "\x1b[38;2;{};{};{}m▄",
                        b[0], b[1], b[2],
                    ));
                    last_was_colored = true;
                }
                (false, false) => {
                    if last_was_colored {
                        line.push_str("\x1b[0m");
                        last_was_colored = false;
                    }
                    line.push(' ');
                }
            }
        }

        line.push_str("\x1b[0m");
        lines.push(line);
        y += 2;
    }

    lines
}

/// Load the full-size Eve banner image as a `DynamicImage`.
///
/// Returns `None` if the embedded PNG fails to decode.
pub fn full_image() -> Option<DynamicImage> {
    image::load_from_memory(EVE_FULL_PNG).ok()
}

/// Load the compact Eve banner image as a `DynamicImage`.
///
/// Returns `None` if the embedded PNG fails to decode.
pub fn compact_image() -> Option<DynamicImage> {
    image::load_from_memory(EVE_COMPACT_PNG).ok()
}

/// Load and render the full-size Eve banner art.
pub fn full_art() -> Vec<String> {
    match image::load_from_memory(EVE_FULL_PNG) {
        Ok(img) => render_halfblocks(&img),
        Err(e) => vec![format!("  [banner art failed: {e}]")],
    }
}

/// Load and render the compact Eve banner art.
pub fn compact_art() -> Vec<String> {
    match image::load_from_memory(EVE_COMPACT_PNG) {
        Ok(img) => render_halfblocks(&img),
        Err(e) => vec![format!("  [banner art failed: {e}]")],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_halfblocks_transparent_image_produces_spaces() {
        let img = DynamicImage::new_rgba8(4, 4);
        let lines = render_halfblocks(&img);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn render_halfblocks_odd_height() {
        let img = DynamicImage::new_rgba8(2, 3);
        let lines = render_halfblocks(&img);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn full_image_loads() {
        assert!(full_image().is_some());
    }

    #[test]
    fn compact_image_loads() {
        assert!(compact_image().is_some());
    }

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
}

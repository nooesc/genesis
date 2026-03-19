//! Half-block pixel art rendering for the Eve banner.
//!
//! Converts embedded PNG images to terminal art using Unicode half-block
//! characters (`▀`, `▄`). The legacy CLI banner path still renders ANSI
//! strings, while the welcome-screen path exposes a crate-agnostic frame model
//! suitable for ratatui conversion in `genesis-tui`.

use std::sync::OnceLock;

use image::{DynamicImage, GenericImageView, Rgba};

/// Embedded Eve art (full size).
const EVE_FULL_PNG: &[u8] = include_bytes!("eve_full.png");

/// Embedded Eve art (compact size).
const EVE_COMPACT_PNG: &[u8] = include_bytes!("eve_compact.png");

/// Embedded welcome animation frames.
const WELCOME_FRAME_01_PNG: &[u8] = include_bytes!("../../assets/welcome/eve_frame_01.png");
const WELCOME_FRAME_02_PNG: &[u8] = include_bytes!("../../assets/welcome/eve_frame_02.png");
const WELCOME_FRAME_03_PNG: &[u8] = include_bytes!("../../assets/welcome/eve_frame_03.png");

/// Number of bundled welcome animation frames.
pub const WELCOME_FRAME_COUNT: usize = 3;

static DECODED_FRAMES: OnceLock<[DynamicImage; 3]> = OnceLock::new();

fn decoded_frames() -> &'static [DynamicImage; 3] {
    DECODED_FRAMES.get_or_init(|| {
        [
            image::load_from_memory(WELCOME_FRAME_01_PNG).expect("embedded frame 1"),
            image::load_from_memory(WELCOME_FRAME_02_PNG).expect("embedded frame 2"),
            image::load_from_memory(WELCOME_FRAME_03_PNG).expect("embedded frame 3"),
        ]
    })
}

/// Alpha threshold below which a pixel is considered transparent.
const ALPHA_THRESHOLD: u8 = 30;

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

impl HalfBlockCell {
    const fn blank() -> Self {
        Self {
            symbol: ' ',
            fg: None,
            bg: None,
        }
    }
}

/// A rendered half-block frame sized in terminal character cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HalfBlockFrame {
    pub width: u16,
    pub height: u16,
    pub lines: Vec<Vec<HalfBlockCell>>,
}

impl HalfBlockFrame {
    fn empty(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            lines: vec![vec![HalfBlockCell::blank(); width as usize]; height as usize],
        }
    }
}

/// Render an RGBA image to half-block terminal art lines with ANSI escapes.
///
/// Each output line represents 2 rows of pixels. For each column:
/// - If both pixels are transparent: space
/// - If top opaque, bottom transparent: ▀ with fg = top color
/// - If top transparent, bottom opaque: ▄ with fg = bottom color
/// - If both opaque: ▀ with fg = top color, bg = bottom color
pub fn render_halfblocks(img: &DynamicImage) -> Vec<String> {
    frame_to_ansi_lines(&render_halfblocks_frame_with(img, source_pixel_color))
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

/// Load one of the bundled welcome animation source images.
pub fn welcome_image(index: usize) -> Option<&'static DynamicImage> {
    decoded_frames().get(index)
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

/// Render all bundled welcome frames to a target terminal size.
pub fn render_welcome_frames(target_width: u16, target_height: u16) -> Vec<HalfBlockFrame> {
    (0..WELCOME_FRAME_COUNT)
        .filter_map(|index| render_welcome_frame(index, target_width, target_height))
        .collect()
}

/// Render one welcome animation frame to a target terminal size.
///
/// The target size is specified in terminal character cells. The underlying
/// image is resized to `width x (height * 2)` pixels so that each half-block
/// cell maps to a top and bottom pixel.
pub fn render_welcome_frame(
    index: usize,
    target_width: u16,
    target_height: u16,
) -> Option<HalfBlockFrame> {
    let img = welcome_image(index)?;
    Some(render_welcome_image(img, target_width, target_height))
}

/// Render a decoded welcome image to a target terminal size using a stylized
/// monochrome palette with a small accent path.
pub fn render_welcome_image(
    img: &DynamicImage,
    target_width: u16,
    target_height: u16,
) -> HalfBlockFrame {
    if target_width == 0 || target_height == 0 {
        return HalfBlockFrame::empty(target_width, target_height);
    }

    let pixel_width = target_width as u32;
    let pixel_height = u32::from(target_height).saturating_mul(2);
    let resized = img.resize_exact(
        pixel_width,
        pixel_height,
        image::imageops::FilterType::Triangle,
    );

    let mut frame = render_halfblocks_frame_with(&resized, welcome_pixel_color);
    frame.width = target_width;
    frame.height = target_height;
    frame
}

fn render_halfblocks_frame_with(
    img: &DynamicImage,
    pixel_mapper: fn(Rgba<u8>) -> Option<RgbColor>,
) -> HalfBlockFrame {
    let (width, height) = img.dimensions();
    let line_count = height.div_ceil(2);
    let mut lines = Vec::with_capacity(line_count as usize);

    let rgba = img.to_rgba8();

    let mut y = 0u32;
    while y < height {
        let mut line = Vec::with_capacity(width as usize);
        for x in 0..width {
            let top = pixel_mapper(*rgba.get_pixel(x, y));
            let bottom = if y + 1 < height {
                pixel_mapper(*rgba.get_pixel(x, y + 1))
            } else {
                None
            };

            line.push(match (top, bottom) {
                (Some(fg), Some(bg)) => HalfBlockCell {
                    symbol: '▀',
                    fg: Some(fg),
                    bg: Some(bg),
                },
                (Some(fg), None) => HalfBlockCell {
                    symbol: '▀',
                    fg: Some(fg),
                    bg: None,
                },
                (None, Some(fg)) => HalfBlockCell {
                    symbol: '▄',
                    fg: Some(fg),
                    bg: None,
                },
                (None, None) => HalfBlockCell::blank(),
            });
        }
        lines.push(line);
        y += 2;
    }

    HalfBlockFrame {
        width: width as u16,
        height: line_count as u16,
        lines,
    }
}

fn frame_to_ansi_lines(frame: &HalfBlockFrame) -> Vec<String> {
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

fn source_pixel_color(pixel: Rgba<u8>) -> Option<RgbColor> {
    if pixel[3] <= ALPHA_THRESHOLD {
        return None;
    }

    Some(RgbColor::new(pixel[0], pixel[1], pixel[2]))
}

fn welcome_pixel_color(pixel: Rgba<u8>) -> Option<RgbColor> {
    if pixel[3] <= ALPHA_THRESHOLD {
        return None;
    }

    let [r, g, b, _] = pixel.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max.saturating_sub(min);
    let luminance =
        ((54u16 * u16::from(r) + 183u16 * u16::from(g) + 19u16 * u16::from(b)) / 256) as u8;

    if is_welcome_background_key(r, g, b, max, chroma, luminance) {
        return None;
    }

    Some(RgbColor::new(r, g, b))
}

fn is_welcome_background_key(_r: u8, _g: u8, _b: u8, max: u8, chroma: u8, luminance: u8) -> bool {
    max <= 14 && chroma <= 10 && luminance <= 12
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

    #[test]
    fn welcome_images_load() {
        for index in 0..WELCOME_FRAME_COUNT {
            assert!(
                welcome_image(index).is_some(),
                "missing welcome frame {index}"
            );
        }
    }

    #[test]
    fn render_welcome_frame_respects_target_size() {
        let frame = render_welcome_frame(0, 24, 12).expect("frame should load");
        assert_eq!(frame.width, 24);
        assert_eq!(frame.height, 12);
        assert_eq!(frame.lines.len(), 12);
        assert!(frame.lines.iter().all(|line| line.len() == 24));
    }

    #[test]
    fn render_welcome_image_preserves_full_frame_without_cropping() {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(10, 10, |x, y| {
            if x == 0 || y == 0 || x == 9 || y == 9 {
                Rgba([255, 255, 255, 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        }));

        let frame = render_welcome_image(&img, 8, 4);
        let left_edge_has_content = frame
            .lines
            .iter()
            .any(|row| row.first().is_some_and(|cell| cell.symbol != ' '));
        let right_edge_has_content = frame
            .lines
            .iter()
            .any(|row| row.last().is_some_and(|cell| cell.symbol != ' '));

        assert!(left_edge_has_content);
        assert!(right_edge_has_content);
    }

    #[test]
    fn render_welcome_frame_preserves_color_detail() {
        let frame = render_welcome_frame(1, 24, 12).expect("frame should load");
        let unique_colors = frame
            .lines
            .iter()
            .flatten()
            .flat_map(|cell| [cell.fg, cell.bg])
            .flatten()
            .collect::<std::collections::BTreeSet<_>>();

        assert!(unique_colors.len() > 8);
    }
}

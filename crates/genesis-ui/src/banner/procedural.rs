//! Procedural composition engine for the welcome screen.
//!
//! Combines geometry layers (hex grid, cruciform, arcs), a bezier silhouette,
//! and a distance-based dissolve effect into a final [`HalfBlockFrame`] suitable
//! for rendering in the terminal.
//!
//! All internal work is done at pixel resolution (`width x height*2`), then
//! converted to half-block cells via [`pixels_to_halfblocks`].

use super::frames::{HalfBlockCell, HalfBlockFrame, RgbColor};
use super::geometry;
use super::silhouette;
use crate::colors::{EVE_AMBER, EVE_DARK, EVE_LAVENDER, EVE_LILAC, EVE_PURPLE};

/// Alpha threshold -- pixels below this are considered transparent.
const ALPHA_THRESHOLD: f32 = 0.05;

/// Hex grid spacing for full-size frames.
const HEX_SPACING_FULL: u32 = 8;

/// Hex grid spacing for compact frames.
const HEX_SPACING_COMPACT: u32 = 12;

/// Compact frame width threshold.
const COMPACT_THRESHOLD: u32 = 35;

/// Dissolve base distance (normalized) -- how far from center the dissolve starts.
const DISSOLVE_BASE: f32 = 0.35;

/// Dissolve oscillation amplitude.
const DISSOLVE_AMPLITUDE: f32 = 0.06;

/// Arc radii for the halo (normalized coordinates).
const ARC_RADII: &[f32] = &[0.12, 0.20, 0.28];

/// Compose a procedural welcome frame.
///
/// `width` and `height` are in terminal character cells (half-block rows).
/// Internally works at pixel resolution: `width x (height * 2)`.
/// `t` is the animation parameter in `[0.0, 1.0]`.
pub fn compose(width: u16, height: u16, t: f64) -> HalfBlockFrame {
    let px_w = width as u32;
    let px_h = (height as u32) * 2;

    if px_w == 0 || px_h == 0 {
        return HalfBlockFrame {
            width,
            height,
            lines: Vec::new(),
        };
    }

    let is_compact = px_w < COMPACT_THRESHOLD;
    let t_f32 = t as f32;

    // ── Layer 1: Hex grid ────────────────────────────────────────────
    let hex_spacing = if is_compact {
        HEX_SPACING_COMPACT
    } else {
        HEX_SPACING_FULL
    };
    let hex_grid = geometry::render_hex_grid(
        px_w,
        px_h,
        hex_spacing,
        RgbColor::from_tuple(EVE_DARK),
        0.5,
        0.25,
        t_f32,
    );

    // ── Layer 2: Cruciform ───────────────────────────────────────────
    let cruciform_t = t_f32 + 0.5;
    let cruciform = geometry::render_cruciform(
        px_w,
        px_h,
        0.5,
        0.25,
        0.015,
        RgbColor::from_tuple(EVE_AMBER),
        cruciform_t,
    );

    // ── Layer 3: Arcs (skip for compact) ─────────────────────────────
    let arcs = if !is_compact {
        let dimmed_amber = geometry::dim_color(RgbColor::from_tuple(EVE_AMBER), 0.5);
        geometry::render_arcs(px_w, px_h, 0.5, 0.25, ARC_RADII, dimmed_amber)
    } else {
        geometry::new_pixel_grid(px_w, px_h)
    };

    // ── Compose background (painter's algorithm) ─────────────────────
    let mut background = hex_grid;
    blend_layers(&mut background, &cruciform);
    blend_layers(&mut background, &arcs);

    // ── Layer 4: Silhouette ──────────────────────────────────────────
    let sil_path = silhouette::default_silhouette();
    let mask = silhouette::rasterize(&sil_path, px_w, px_h);
    let mut sil_pixels = silhouette::fill_gradient(
        &mask,
        px_w,
        px_h,
        RgbColor::from_tuple(EVE_LAVENDER),
        RgbColor::from_tuple(EVE_PURPLE),
    );

    // ── Apply dissolve to silhouette ─────────────────────────────────
    apply_dissolve(&mut sil_pixels, &mask, px_w, px_h, t);

    // ── Final composite: silhouette on top of background ─────────────
    blend_layers(&mut background, &sil_pixels);

    // ── Convert to half-blocks ───────────────────────────────────────
    pixels_to_halfblocks(&background, width, height)
}

/// Convert a pixel grid to a [`HalfBlockFrame`] by pairing rows into half-block cells.
///
/// Iterates pixels in pairs of rows (y, y+1). For each column the top and bottom
/// pixels are combined:
/// - `(Some(fg), Some(bg))` -> upper half-block with fg/bg colors
/// - `(Some(fg), None)` -> upper half-block with fg only
/// - `(None, Some(fg))` -> lower half-block with fg only
/// - `(None, None)` -> space (blank)
fn pixels_to_halfblocks(
    pixels: &[Vec<Option<RgbColor>>],
    width: u16,
    height: u16,
) -> HalfBlockFrame {
    let mut lines = Vec::with_capacity(height as usize);

    let mut y = 0usize;
    let total_pixel_rows = pixels.len();

    while y < total_pixel_rows && lines.len() < height as usize {
        let mut line = Vec::with_capacity(width as usize);

        for x in 0..width as usize {
            let top = pixels[y].get(x).copied().flatten();
            let bottom = if y + 1 < total_pixel_rows {
                pixels[y + 1].get(x).copied().flatten()
            } else {
                None
            };

            line.push(match (top, bottom) {
                (Some(fg), Some(bg)) => HalfBlockCell {
                    symbol: '\u{2580}', // ▀
                    fg: Some(fg),
                    bg: Some(bg),
                },
                (Some(fg), None) => HalfBlockCell {
                    symbol: '\u{2580}', // ▀
                    fg: Some(fg),
                    bg: None,
                },
                (None, Some(fg)) => HalfBlockCell {
                    symbol: '\u{2584}', // ▄
                    fg: Some(fg),
                    bg: None,
                },
                (None, None) => HalfBlockCell {
                    symbol: ' ',
                    fg: None,
                    bg: None,
                },
            });
        }

        lines.push(line);
        y += 2;
    }

    HalfBlockFrame {
        width,
        height,
        lines,
    }
}

/// Apply dissolve effect to the silhouette layer.
///
/// Pixels beyond a distance threshold from the figure center are erased.
/// Pixels near the threshold edge get a deterministic sparkle pattern using
/// `EVE_LILAC`.
fn apply_dissolve(
    silhouette: &mut [Vec<Option<RgbColor>>],
    mask: &[Vec<f32>],
    width: u32,
    height: u32,
    t: f64,
) {
    use std::f64::consts::TAU;

    let threshold = DISSOLVE_BASE + DISSOLVE_AMPLITUDE * (t * TAU).sin() as f32;
    let lilac = RgbColor::from_tuple(EVE_LILAC);

    let w = width as f32;
    let h = height as f32;

    for y in 0..height as usize {
        for x in 0..width as usize {
            let mask_val = mask
                .get(y)
                .and_then(|row| row.get(x))
                .copied()
                .unwrap_or(0.0);

            if mask_val <= ALPHA_THRESHOLD {
                continue;
            }

            let nx = x as f32 / w;
            let ny = y as f32 / h;
            let dx = nx - 0.5;
            let dy = ny - 0.4;
            let dist = (dx * dx + dy * dy).sqrt();

            if dist > threshold {
                // Beyond dissolve boundary -- erase.
                silhouette[y][x] = None;
            } else if (dist - threshold).abs() < 0.03 {
                // Edge zone -- deterministic sparkle pattern.
                let sparkle = ((x * 7 + y * 13) % 5) < 2;
                if sparkle {
                    silhouette[y][x] = Some(lilac);
                }
            }
        }
    }
}

/// Blend a foreground layer onto a background layer (painter's algorithm).
///
/// For each pixel: if the foreground is `Some`, it overwrites the background.
/// Otherwise the background pixel is kept.
fn blend_layers(
    background: &mut [Vec<Option<RgbColor>>],
    foreground: &[Vec<Option<RgbColor>>],
) {
    for (bg_row, fg_row) in background.iter_mut().zip(foreground.iter()) {
        for (bg_px, fg_px) in bg_row.iter_mut().zip(fg_row.iter()) {
            if fg_px.is_some() {
                *bg_px = *fg_px;
            }
        }
    }
}

/// Linearly interpolate between two colors.
#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn lerp_color(a: RgbColor, b: RgbColor, t: f32) -> RgbColor {
    let t = t.clamp(0.0, 1.0);
    let r = f32::from(a.r) + (f32::from(b.r) - f32::from(a.r)) * t;
    let g = f32::from(a.g) + (f32::from(b.g) - f32::from(a.g)) * t;
    let bl = f32::from(a.b) + (f32::from(b.b) - f32::from(a.b)) * t;
    RgbColor::new(r as u8, g as u8, bl as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn compose_produces_correct_dimensions() {
        let frame = compose(40, 35, 0.0);
        assert_eq!(frame.width, 40);
        assert_eq!(frame.height, 35);
        assert_eq!(frame.lines.len(), 35);
        for (i, line) in frame.lines.iter().enumerate() {
            assert_eq!(
                line.len(),
                40,
                "line {i} has {} cells, expected 40",
                line.len()
            );
        }
    }

    #[test]
    fn compose_zero_size() {
        let frame = compose(0, 0, 0.0);
        assert_eq!(frame.width, 0);
        assert_eq!(frame.height, 0);
        assert!(frame.lines.is_empty());
    }

    #[test]
    fn compose_has_multiple_colors() {
        let frame = compose(40, 35, 0.0);
        let unique_colors: HashSet<_> = frame
            .lines
            .iter()
            .flatten()
            .flat_map(|cell| [cell.fg, cell.bg])
            .flatten()
            .collect();
        assert!(
            unique_colors.len() > 8,
            "expected >8 unique colors, got {}",
            unique_colors.len()
        );
    }

    #[test]
    fn compose_different_t_values_differ() {
        let frame_a = compose(40, 35, 0.0);
        let frame_b = compose(40, 35, 0.25);
        // At least one cell should differ between the two animation states.
        let differs = frame_a
            .lines
            .iter()
            .zip(frame_b.lines.iter())
            .any(|(row_a, row_b)| row_a.iter().zip(row_b.iter()).any(|(a, b)| a != b));
        assert!(differs, "frames at t=0.0 and t=0.5 should not be identical");
    }

    #[test]
    fn pixels_to_halfblocks_basic() {
        let red = Some(RgbColor::new(255, 0, 0));
        let green = Some(RgbColor::new(0, 255, 0));
        let blue = Some(RgbColor::new(0, 0, 255));

        // 4x4 pixel grid -> 4 columns x 2 half-block rows
        let pixels = vec![
            vec![red, green, None, None],   // row 0 (top of halfblock row 0)
            vec![blue, None, red, None],     // row 1 (bottom of halfblock row 0)
            vec![None, None, green, blue],   // row 2 (top of halfblock row 1)
            vec![None, red, None, None],     // row 3 (bottom of halfblock row 1)
        ];

        let frame = pixels_to_halfblocks(&pixels, 4, 2);
        assert_eq!(frame.width, 4);
        assert_eq!(frame.height, 2);
        assert_eq!(frame.lines.len(), 2);
        assert_eq!(frame.lines[0].len(), 4);
        assert_eq!(frame.lines[1].len(), 4);

        // Column 0, row 0: top=red, bottom=blue -> upper half-block with fg+bg
        assert_eq!(frame.lines[0][0].symbol, '\u{2580}');
        assert_eq!(frame.lines[0][0].fg, red);
        assert_eq!(frame.lines[0][0].bg, blue);

        // Column 1, row 0: top=green, bottom=None -> upper half-block, fg only
        assert_eq!(frame.lines[0][1].symbol, '\u{2580}');
        assert_eq!(frame.lines[0][1].fg, green);
        assert_eq!(frame.lines[0][1].bg, None);

        // Column 2, row 0: top=None, bottom=red -> lower half-block
        assert_eq!(frame.lines[0][2].symbol, '\u{2584}');
        assert_eq!(frame.lines[0][2].fg, red);

        // Column 3, row 0: top=None, bottom=None -> blank space
        assert_eq!(frame.lines[0][3].symbol, ' ');
        assert_eq!(frame.lines[0][3].fg, None);
        assert_eq!(frame.lines[0][3].bg, None);

        // Column 1, row 1: top=None, bottom=red -> lower half-block
        assert_eq!(frame.lines[1][1].symbol, '\u{2584}');
        assert_eq!(frame.lines[1][1].fg, red);
    }

    #[test]
    fn blend_layers_foreground_overwrites() {
        let red = Some(RgbColor::new(255, 0, 0));
        let green = Some(RgbColor::new(0, 255, 0));

        let mut bg = vec![vec![red, None, red]];
        let fg = vec![vec![green, green, None]];

        blend_layers(&mut bg, &fg);

        // Foreground overwrites where it is Some.
        assert_eq!(bg[0][0], green); // red overwritten by green
        assert_eq!(bg[0][1], green); // None overwritten by green
        assert_eq!(bg[0][2], red);   // kept because fg is None
    }

    #[test]
    fn compose_compact_size() {
        // Width 20 < COMPACT_THRESHOLD (35), so arcs should be skipped.
        let frame = compose(20, 18, 0.0);
        assert_eq!(frame.width, 20);
        assert_eq!(frame.height, 18);
        assert_eq!(frame.lines.len(), 18);
        for line in &frame.lines {
            assert_eq!(line.len(), 20);
        }
    }

    #[test]
    fn lerp_color_endpoints() {
        let black = RgbColor::new(0, 0, 0);
        let white = RgbColor::new(255, 255, 255);

        let at_zero = lerp_color(black, white, 0.0);
        assert_eq!(at_zero, black);

        let at_one = lerp_color(black, white, 1.0);
        assert_eq!(at_one, white);
    }

    #[test]
    fn lerp_color_midpoint() {
        let black = RgbColor::new(0, 0, 0);
        let white = RgbColor::new(254, 254, 254);

        let mid = lerp_color(black, white, 0.5);
        assert_eq!(mid.r, 127);
        assert_eq!(mid.g, 127);
        assert_eq!(mid.b, 127);
    }
}

//! Procedural geometric pattern rendering for the welcome screen.
//!
//! All functions produce pixel grids (`Vec<Vec<Option<RgbColor>>>`) indexed as
//! `pixels[y][x]`. The composition engine converts these to half-block cells.

use std::f32::consts::TAU;

use super::frames::RgbColor;
pub fn new_pixel_grid(width: u32, height: u32) -> Vec<Vec<Option<RgbColor>>> {
    vec![vec![None; width as usize]; height as usize]
}

#[allow(clippy::cast_possible_truncation)]
pub fn brighten(color: RgbColor, factor: f32) -> RgbColor {
    RgbColor::new(
        (f32::from(color.r) * factor).min(255.0) as u8,
        (f32::from(color.g) * factor).min(255.0) as u8,
        (f32::from(color.b) * factor).min(255.0) as u8,
    )
}

#[allow(clippy::cast_possible_truncation)]
pub fn dim_color(color: RgbColor, factor: f32) -> RgbColor {
    let f = factor.clamp(0.0, 1.0);
    RgbColor::new(
        (f32::from(color.r) * f) as u8,
        (f32::from(color.g) * f) as u8,
        (f32::from(color.b) * f) as u8,
    )
}

#[allow(clippy::cast_possible_truncation)]
pub fn render_cruciform(
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    arm_half_width: f32,
    color: RgbColor,
    brightness_t: f32,
) -> Vec<Vec<Option<RgbColor>>> {
    if width == 0 || height == 0 {
        return new_pixel_grid(width, height);
    }

    let mut grid = new_pixel_grid(width, height);
    let brightness = 0.85 + 0.15 * (brightness_t * TAU).sin();
    let base = brighten(color, brightness);

    let w = width as f32;
    let h = height as f32;
    let cx_px = center_x * w;
    let cy_px = center_y * h;
    let ahw_x = arm_half_width * w;
    let ahw_y = arm_half_width * h;

    let h_arm_half_len = w * 0.30;
    let diag_len = h_arm_half_len * 0.5;

    for py in 0..height {
        for px in 0..width {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            let dx = (fx - cx_px).abs();
            let dy = (fy - cy_px).abs();

            let on_vertical = dx <= ahw_x;
            let on_horizontal = dy <= ahw_y && dx <= h_arm_half_len;

            let on_diagonal = {
                let d1 = ((fx - cx_px) - (fy - cy_px)).abs();
                let d2 = ((fx - cx_px) + (fy - cy_px)).abs();
                let diag_dist = ((fx - cx_px).powi(2) + (fy - cy_px).powi(2)).sqrt();
                diag_dist <= diag_len && (d1 <= 1.0 || d2 <= 1.0)
            };

            if on_vertical || on_horizontal || on_diagonal {
                let edge_dist_v = if on_vertical {
                    1.0 - (dx / ahw_x).min(1.0)
                } else {
                    1.0
                };
                let edge_dist_h = if on_horizontal {
                    1.0 - (dy / ahw_y).min(1.0)
                } else {
                    1.0
                };

                let edge_factor = if on_vertical && !on_horizontal && !on_diagonal {
                    0.7 + 0.3 * edge_dist_v
                } else if on_horizontal && !on_vertical && !on_diagonal {
                    0.7 + 0.3 * edge_dist_h
                } else if on_diagonal && !on_vertical && !on_horizontal {
                    0.6
                } else {
                    1.0
                };

                grid[py as usize][px as usize] = Some(dim_color(base, edge_factor));
            }
        }
    }

    grid
}

#[allow(clippy::cast_possible_truncation)]
pub fn render_arcs(
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    radii: &[f32],
    color: RgbColor,
) -> Vec<Vec<Option<RgbColor>>> {
    if width == 0 || height == 0 || radii.is_empty() {
        return new_pixel_grid(width, height);
    }

    let mut grid = new_pixel_grid(width, height);
    let w = width as f32;
    let h = height as f32;
    let cx_px = center_x * w;
    let cy_px = center_y * h;
    let diag = (w * w + h * h).sqrt();

    for py in 0..height {
        let fy = py as f32 + 0.5;
        if fy >= cy_px {
            continue;
        }

        for px in 0..width {
            let fx = px as f32 + 0.5;
            let dist = ((fx - cx_px).powi(2) + (fy - cy_px).powi(2)).sqrt();

            for (i, &radius) in radii.iter().enumerate() {
                let radius_px = radius * diag;
                if (dist - radius_px).abs() < 0.7 {
                    let dim_factor = 0.7_f32.powi(i as i32);
                    let c = dim_color(color, dim_factor);
                    grid[py as usize][px as usize] = Some(c);
                    break;
                }
            }
        }
    }

    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_COLOR: RgbColor = RgbColor::new(200, 150, 100);

    #[test]
    fn cruciform_centered() {
        let grid = render_cruciform(100, 60, 0.5, 0.5, 0.02, TEST_COLOR, 0.0);
        assert!(grid[30][50].is_some());
    }

    #[test]
    fn cruciform_has_vertical_arm() {
        let grid = render_cruciform(100, 60, 0.5, 0.5, 0.02, TEST_COLOR, 0.0);
        let vertical_pixels: usize = (0..60).filter(|&y| grid[y][50].is_some()).count();
        assert!(vertical_pixels >= 50);
    }

    #[test]
    fn arcs_upper_hemisphere_only() {
        let grid = render_arcs(100, 60, 0.5, 0.5, &[0.15, 0.25, 0.35], TEST_COLOR);
        let below_count = grid[30..]
            .iter()
            .flatten()
            .filter(|p| p.is_some())
            .count();
        assert_eq!(below_count, 0);
    }

    #[test]
    fn brighten_clamps_to_255() {
        let bright = brighten(RgbColor::new(200, 200, 200), 2.0);
        assert_eq!(bright, RgbColor::new(255, 255, 255));
    }

    #[test]
    fn dim_color_reduces() {
        let grey = dim_color(RgbColor::new(255, 255, 255), 0.5);
        assert_eq!(grey.r, 127);
    }
}

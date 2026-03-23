//! Scanline-based feminine silhouette for the welcome screen.
//!
//! Defines the figure as a symmetric profile of half-widths at key y-positions,
//! then interpolates for any target resolution. This produces crisp, recognizable
//! shapes even at 20px wide — unlike bezier curves which turn to mush at low res.

use super::frames::RgbColor;

/// A profile entry: normalized y-position and half-width from center (x=0.5).
struct ProfilePoint {
    y: f32,
    half_w: f32,
}

/// The silhouette profile — half-widths at key y-positions.
///
/// The figure is symmetric around x=0.5. At each y, the filled region
/// spans from `0.5 - half_w` to `0.5 + half_w`.
///
/// Designed to produce a clearly recognizable feminine silhouette at 20-80px:
/// round head, narrow neck, wide shoulders, hourglass waist, wide hips.
const PROFILE: &[ProfilePoint] = &[
    ProfilePoint { y: 0.06, half_w: 0.00 },  // top of head (point)
    ProfilePoint { y: 0.08, half_w: 0.05 },  // head widening
    ProfilePoint { y: 0.11, half_w: 0.08 },  // widest head
    ProfilePoint { y: 0.14, half_w: 0.08 },  // head round
    ProfilePoint { y: 0.17, half_w: 0.06 },  // jaw narrowing
    ProfilePoint { y: 0.19, half_w: 0.04 },  // chin
    ProfilePoint { y: 0.21, half_w: 0.025 }, // neck (very narrow!)
    ProfilePoint { y: 0.24, half_w: 0.025 }, // neck
    ProfilePoint { y: 0.27, half_w: 0.10 },  // shoulder sweep starts
    ProfilePoint { y: 0.30, half_w: 0.20 },  // shoulders wide
    ProfilePoint { y: 0.33, half_w: 0.23 },  // shoulder peak / upper arm
    ProfilePoint { y: 0.36, half_w: 0.21 },  // upper arms
    ProfilePoint { y: 0.40, half_w: 0.17 },  // arms curving in
    ProfilePoint { y: 0.44, half_w: 0.13 },  // upper waist
    ProfilePoint { y: 0.48, half_w: 0.11 },  // waist (narrowest!)
    ProfilePoint { y: 0.52, half_w: 0.14 },  // waist-to-hip transition
    ProfilePoint { y: 0.56, half_w: 0.20 },  // hips widening
    ProfilePoint { y: 0.60, half_w: 0.22 },  // widest hips
    ProfilePoint { y: 0.64, half_w: 0.20 },  // hips narrowing
    ProfilePoint { y: 0.68, half_w: 0.16 },  // upper thighs
    ProfilePoint { y: 0.72, half_w: 0.13 },  // thighs
    ProfilePoint { y: 0.76, half_w: 0.10 },  // lower thighs
    ProfilePoint { y: 0.80, half_w: 0.07 },  // calves / dissolve zone
    ProfilePoint { y: 0.85, half_w: 0.04 },  // dissolving
    ProfilePoint { y: 0.90, half_w: 0.00 },  // fully dissolved
];

/// Rasterize the silhouette into an alpha mask at the given pixel dimensions.
///
/// Returns a grid of `height x width` with values 0.0 (outside) to 1.0 (inside).
/// Edge pixels get fractional alpha for anti-aliasing.
pub fn rasterize(width: u32, height: u32) -> Vec<Vec<f32>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    let w = width as f32;
    let h = height as f32;
    let mut mask = vec![vec![0.0f32; width as usize]; height as usize];

    for row in 0..height {
        let ny = (row as f32 + 0.5) / h;
        let half_w = interpolate_profile(ny);

        if half_w <= 0.0 {
            continue;
        }

        let left = (0.5 - half_w) * w;
        let right = (0.5 + half_w) * w;

        for col in 0..width {
            let cx = col as f32 + 0.5;

            if cx >= left && cx <= right {
                // Compute coverage for anti-aliasing at edges.
                let left_edge = (cx - left).min(1.0).max(0.0);
                let right_edge = (right - cx).min(1.0).max(0.0);
                mask[row as usize][col as usize] = left_edge.min(right_edge).min(1.0);
            }
        }
    }

    mask
}

/// Interpolate the profile half-width at a given normalized y position.
fn interpolate_profile(y: f32) -> f32 {
    if y <= PROFILE[0].y {
        return PROFILE[0].half_w;
    }
    let last = PROFILE.len() - 1;
    if y >= PROFILE[last].y {
        return PROFILE[last].half_w;
    }

    for i in 0..last {
        let a = &PROFILE[i];
        let b = &PROFILE[i + 1];
        if y >= a.y && y <= b.y {
            let t = (y - a.y) / (b.y - a.y);
            // Smooth interpolation (smoothstep) for rounder transitions.
            let t = t * t * (3.0 - 2.0 * t);
            return a.half_w + (b.half_w - a.half_w) * t;
        }
    }

    0.0
}

/// Apply a vertical gradient fill to the silhouette mask.
///
/// Pixels with `mask[y][x] > 0.0` get a color lerped between `top_color` and
/// `bottom_color` based on row position, modulated by the alpha value.
pub fn fill_gradient(
    mask: &[Vec<f32>],
    width: u32,
    height: u32,
    top_color: RgbColor,
    bottom_color: RgbColor,
) -> Vec<Vec<Option<RgbColor>>> {
    let h = height.max(1) as f32;

    mask.iter()
        .enumerate()
        .map(|(y, row)| {
            let t = y as f32 / (h - 1.0).max(1.0);
            let gradient = lerp_color(top_color, bottom_color, t);

            row.iter()
                .take(width as usize)
                .map(|&alpha| {
                    if alpha <= 0.0 {
                        None
                    } else {
                        Some(lerp_color(RgbColor::new(0, 0, 0), gradient, alpha))
                    }
                })
                .collect()
        })
        .collect()
}

fn lerp_color(a: RgbColor, b: RgbColor, t: f32) -> RgbColor {
    let t = t.clamp(0.0, 1.0);
    let r = a.r as f32 + (b.r as f32 - a.r as f32) * t;
    let g = a.g as f32 + (b.g as f32 - a.g as f32) * t;
    let bl = a.b as f32 + (b.b as f32 - a.b as f32) * t;
    RgbColor::new(r as u8, g as u8, bl as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_starts_and_ends_at_zero() {
        assert_eq!(PROFILE[0].half_w, 0.0);
        assert_eq!(PROFILE[PROFILE.len() - 1].half_w, 0.0);
    }

    #[test]
    fn profile_has_hourglass_shape() {
        // Shoulders should be wider than waist, hips wider than waist.
        let shoulder_w = interpolate_profile(0.33);
        let waist_w = interpolate_profile(0.48);
        let hip_w = interpolate_profile(0.60);
        assert!(
            shoulder_w > waist_w,
            "shoulders ({shoulder_w}) should be wider than waist ({waist_w})"
        );
        assert!(
            hip_w > waist_w,
            "hips ({hip_w}) should be wider than waist ({waist_w})"
        );
    }

    #[test]
    fn profile_neck_is_narrowest_body_part() {
        let neck_w = interpolate_profile(0.22);
        let waist_w = interpolate_profile(0.48);
        assert!(
            neck_w < waist_w,
            "neck ({neck_w}) should be narrower than waist ({waist_w})"
        );
    }

    #[test]
    fn rasterize_has_content() {
        let mask = rasterize(40, 70);
        assert_eq!(mask.len(), 70);
        let filled = mask.iter().flat_map(|r| r.iter()).filter(|&&v| v > 0.0).count();
        assert!(filled > 0);
    }

    #[test]
    fn rasterize_center_is_filled() {
        let mask = rasterize(40, 70);
        let val = mask[35][20];
        assert!(val > 0.5, "center pixel should be filled, got {val}");
    }

    #[test]
    fn rasterize_shoulders_are_wide() {
        let mask = rasterize(40, 70);
        // At shoulder height (~y=0.33 -> row 23)
        let shoulder_row = &mask[23];
        let filled = shoulder_row.iter().filter(|&&v| v > 0.0).count();
        assert!(
            filled >= 14,
            "shoulders should span >=14px at width 40, got {filled}"
        );
    }

    #[test]
    fn rasterize_zero_size() {
        assert!(rasterize(0, 0).is_empty());
        assert!(rasterize(10, 0).is_empty());
        assert!(rasterize(0, 10).is_empty());
    }

    #[test]
    fn fill_gradient_top_bottom_differ() {
        let mask = rasterize(40, 70);
        let top_color = RgbColor::new(200, 100, 255);
        let bottom_color = RgbColor::new(50, 150, 200);
        let filled = fill_gradient(&mask, 40, 70, top_color, bottom_color);

        let top_pixel = filled[10..20]
            .iter()
            .flat_map(|row| row.iter())
            .find_map(|c| c.as_ref())
            .expect("expected a filled pixel in top region");

        let bottom_pixel = filled[45..60]
            .iter()
            .flat_map(|row| row.iter())
            .find_map(|c| c.as_ref())
            .expect("expected a filled pixel in bottom region");

        assert_ne!(top_pixel, bottom_pixel);
    }
}

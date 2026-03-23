//! Procedural feminine silhouette shape defined by cubic bezier curves.
//!
//! The silhouette is abstract and archetypal — no facial features, deliberately
//! symbol-like. It lives in normalized [0,1] coordinate space and can be
//! rasterized to any pixel grid size.

use super::frames::RgbColor;

/// A 2D point in normalized [0,1] coordinate space.
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

/// A cubic bezier curve segment.
#[derive(Debug, Clone, Copy)]
pub struct CubicBezier {
    pub p0: Point,
    pub p1: Point, // control point 1
    pub p2: Point, // control point 2
    pub p3: Point,
}

/// The complete silhouette outline as a closed path of bezier segments.
pub struct SilhouettePath {
    pub segments: Vec<CubicBezier>,
}

/// Returns the Eve silhouette — an abstract feminine form.
///
/// The path is a closed loop of cubic bezier segments in normalized [0,1]
/// space. The figure is bold and exaggerated to read clearly at low pixel
/// resolutions (20-40px wide). Centered at x=0.5, fills ~50% of width.
pub fn default_silhouette() -> SilhouettePath {
    // The figure needs to be WIDE to read at half-block resolution.
    // At 20px wide, a 50% fill gives ~10px across at shoulders/hips.
    //
    // Key proportions (exaggerated for low-res):
    //   - Head: rounded, ~20% of width
    //   - Neck: narrow pinch (~12% of width)
    //   - Shoulders: wide sweep (~50% of width)
    //   - Waist: narrow (~25% of width)
    //   - Hips: wide (~48% of width)
    //   - Lower body: tapers to dissolve

    let segments = vec![
        // === TOP OF HEAD (starting at crown, going right) ===
        // Crown to right temple
        CubicBezier {
            p0: Point { x: 0.50, y: 0.10 },
            p1: Point { x: 0.54, y: 0.08 },
            p2: Point { x: 0.58, y: 0.08 },
            p3: Point { x: 0.60, y: 0.11 },
        },
        // Right temple down to right jaw
        CubicBezier {
            p0: Point { x: 0.60, y: 0.11 },
            p1: Point { x: 0.62, y: 0.14 },
            p2: Point { x: 0.61, y: 0.18 },
            p3: Point { x: 0.58, y: 0.21 },
        },
        // Right jaw to right neck (narrow pinch)
        CubicBezier {
            p0: Point { x: 0.58, y: 0.21 },
            p1: Point { x: 0.56, y: 0.23 },
            p2: Point { x: 0.55, y: 0.24 },
            p3: Point { x: 0.54, y: 0.26 },
        },
        // Right neck to right shoulder (wide sweep outward)
        CubicBezier {
            p0: Point { x: 0.54, y: 0.26 },
            p1: Point { x: 0.58, y: 0.27 },
            p2: Point { x: 0.68, y: 0.29 },
            p3: Point { x: 0.73, y: 0.32 },
        },
        // Right shoulder to right upper arm
        CubicBezier {
            p0: Point { x: 0.73, y: 0.32 },
            p1: Point { x: 0.76, y: 0.34 },
            p2: Point { x: 0.76, y: 0.37 },
            p3: Point { x: 0.74, y: 0.40 },
        },
        // Right upper arm curving in to right waist
        CubicBezier {
            p0: Point { x: 0.74, y: 0.40 },
            p1: Point { x: 0.70, y: 0.44 },
            p2: Point { x: 0.66, y: 0.47 },
            p3: Point { x: 0.62, y: 0.50 },
        },
        // Right waist to right hip (flare out)
        CubicBezier {
            p0: Point { x: 0.62, y: 0.50 },
            p1: Point { x: 0.60, y: 0.52 },
            p2: Point { x: 0.70, y: 0.56 },
            p3: Point { x: 0.72, y: 0.60 },
        },
        // Right hip curving down
        CubicBezier {
            p0: Point { x: 0.72, y: 0.60 },
            p1: Point { x: 0.73, y: 0.63 },
            p2: Point { x: 0.70, y: 0.68 },
            p3: Point { x: 0.66, y: 0.72 },
        },
        // Right thigh to dissolve
        CubicBezier {
            p0: Point { x: 0.66, y: 0.72 },
            p1: Point { x: 0.62, y: 0.76 },
            p2: Point { x: 0.57, y: 0.80 },
            p3: Point { x: 0.53, y: 0.84 },
        },
        // === BOTTOM (right to left) ===
        CubicBezier {
            p0: Point { x: 0.53, y: 0.84 },
            p1: Point { x: 0.51, y: 0.85 },
            p2: Point { x: 0.49, y: 0.85 },
            p3: Point { x: 0.47, y: 0.84 },
        },
        // === LEFT SIDE (bottom to top, mirrored) ===
        // Left dissolve up to left thigh
        CubicBezier {
            p0: Point { x: 0.47, y: 0.84 },
            p1: Point { x: 0.43, y: 0.80 },
            p2: Point { x: 0.38, y: 0.76 },
            p3: Point { x: 0.34, y: 0.72 },
        },
        // Left thigh up to left hip
        CubicBezier {
            p0: Point { x: 0.34, y: 0.72 },
            p1: Point { x: 0.30, y: 0.68 },
            p2: Point { x: 0.27, y: 0.63 },
            p3: Point { x: 0.28, y: 0.60 },
        },
        // Left hip to left waist
        CubicBezier {
            p0: Point { x: 0.28, y: 0.60 },
            p1: Point { x: 0.30, y: 0.56 },
            p2: Point { x: 0.40, y: 0.52 },
            p3: Point { x: 0.38, y: 0.50 },
        },
        // Left waist up to left upper arm
        CubicBezier {
            p0: Point { x: 0.38, y: 0.50 },
            p1: Point { x: 0.34, y: 0.47 },
            p2: Point { x: 0.30, y: 0.44 },
            p3: Point { x: 0.26, y: 0.40 },
        },
        // Left upper arm to left shoulder
        CubicBezier {
            p0: Point { x: 0.26, y: 0.40 },
            p1: Point { x: 0.24, y: 0.37 },
            p2: Point { x: 0.24, y: 0.34 },
            p3: Point { x: 0.27, y: 0.32 },
        },
        // Left shoulder to left neck
        CubicBezier {
            p0: Point { x: 0.27, y: 0.32 },
            p1: Point { x: 0.32, y: 0.29 },
            p2: Point { x: 0.42, y: 0.27 },
            p3: Point { x: 0.46, y: 0.26 },
        },
        // Left neck to left jaw
        CubicBezier {
            p0: Point { x: 0.46, y: 0.26 },
            p1: Point { x: 0.45, y: 0.24 },
            p2: Point { x: 0.44, y: 0.23 },
            p3: Point { x: 0.42, y: 0.21 },
        },
        // Left jaw up to left temple
        CubicBezier {
            p0: Point { x: 0.42, y: 0.21 },
            p1: Point { x: 0.39, y: 0.18 },
            p2: Point { x: 0.38, y: 0.14 },
            p3: Point { x: 0.40, y: 0.11 },
        },
        // Left temple to crown (closing)
        CubicBezier {
            p0: Point { x: 0.40, y: 0.11 },
            p1: Point { x: 0.42, y: 0.08 },
            p2: Point { x: 0.46, y: 0.08 },
            p3: Point { x: 0.50, y: 0.10 },
        },
    ];

    SilhouettePath { segments }
}

/// Evaluate a cubic bezier curve at parameter `t` in [0,1].
///
/// B(t) = (1-t)^3 * P0 + 3(1-t)^2 * t * P1 + 3(1-t) * t^2 * P2 + t^3 * P3
pub fn evaluate_bezier(curve: &CubicBezier, t: f32) -> Point {
    let u = 1.0 - t;
    let u2 = u * u;
    let u3 = u2 * u;
    let t2 = t * t;
    let t3 = t2 * t;

    Point {
        x: u3 * curve.p0.x
            + 3.0 * u2 * t * curve.p1.x
            + 3.0 * u * t2 * curve.p2.x
            + t3 * curve.p3.x,
        y: u3 * curve.p0.y
            + 3.0 * u2 * t * curve.p1.y
            + 3.0 * u * t2 * curve.p2.y
            + t3 * curve.p3.y,
    }
}

/// Rasterize the silhouette path into an alpha mask.
///
/// Returns a grid of size `height x width` with values from 0.0 (outside) to
/// 1.0 (inside). Edge pixels use 2x2 sub-pixel sampling for anti-aliasing.
pub fn rasterize(path: &SilhouettePath, width: u32, height: u32) -> Vec<Vec<f32>> {
    if width == 0 || height == 0 {
        return Vec::new();
    }

    // Sample bezier segments into polygon vertices.
    let samples_per_segment = 50;
    let mut vertices: Vec<(f32, f32)> =
        Vec::with_capacity(path.segments.len() * samples_per_segment);

    for segment in &path.segments {
        for i in 0..samples_per_segment {
            let t = i as f32 / samples_per_segment as f32;
            let p = evaluate_bezier(segment, t);
            vertices.push((p.x, p.y));
        }
    }

    let w = width as f32;
    let h = height as f32;

    // Convert normalized vertices to pixel coordinates.
    let pixel_vertices: Vec<(f32, f32)> = vertices.iter().map(|&(x, y)| (x * w, y * h)).collect();

    let mut mask = vec![vec![0.0f32; width as usize]; height as usize];

    for row in 0..height {
        for col in 0..width {
            let cx = col as f32 + 0.5;
            let cy = row as f32 + 0.5;

            // 2x2 sub-pixel sampling for anti-aliasing.
            let offsets: [(f32, f32); 4] =
                [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
            let inside_count = offsets
                .iter()
                .filter(|&&(dx, dy)| {
                    point_in_polygon(col as f32 + dx, row as f32 + dy, &pixel_vertices)
                })
                .count();

            if inside_count > 0 {
                mask[row as usize][col as usize] = inside_count as f32 / 4.0;
            } else if point_in_polygon(cx, cy, &pixel_vertices) {
                mask[row as usize][col as usize] = 1.0;
            }
        }
    }

    mask
}

/// Apply a vertical gradient fill to the silhouette mask.
///
/// Pixels with `mask[y][x] > 0.0` are filled by lerping between `top_color`
/// and `bottom_color` based on the row position. The alpha value from the mask
/// modulates the final color intensity. Returns `None` for fully transparent
/// pixels.
pub fn fill_gradient(
    mask: &[Vec<f32>],
    width: u32,
    height: u32,
    top_color: RgbColor,
    bottom_color: RgbColor,
) -> Vec<Vec<Option<RgbColor>>> {
    let h = if height == 0 { 1.0 } else { height as f32 };

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
                        // Modulate color by alpha for anti-aliased edges.
                        Some(lerp_color(RgbColor::new(0, 0, 0), gradient, alpha))
                    }
                })
                .collect()
        })
        .collect()
}

/// Ray-casting point-in-polygon test.
fn point_in_polygon(px: f32, py: f32, vertices: &[(f32, f32)]) -> bool {
    let n = vertices.len();
    if n < 3 {
        return false;
    }

    let mut inside = false;
    let mut j = n - 1;

    for i in 0..n {
        let (xi, yi) = vertices[i];
        let (xj, yj) = vertices[j];

        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }

        j = i;
    }

    inside
}

/// Linear interpolation between two colors. `t` is clamped to [0,1].
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
    fn bezier_evaluation_endpoints() {
        let curve = CubicBezier {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: 0.25, y: 0.5 },
            p2: Point { x: 0.75, y: 0.5 },
            p3: Point { x: 1.0, y: 1.0 },
        };

        let start = evaluate_bezier(&curve, 0.0);
        assert!((start.x - 0.0).abs() < 1e-6);
        assert!((start.y - 0.0).abs() < 1e-6);

        let end = evaluate_bezier(&curve, 1.0);
        assert!((end.x - 1.0).abs() < 1e-6);
        assert!((end.y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bezier_evaluation_midpoint() {
        let curve = CubicBezier {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: 0.0, y: 1.0 },
            p2: Point { x: 1.0, y: 0.0 },
            p3: Point { x: 1.0, y: 1.0 },
        };

        let mid = evaluate_bezier(&curve, 0.5);
        assert!((mid.x - 0.5).abs() < 1e-5);
        assert!((mid.y - 0.5).abs() < 1e-5);
    }

    #[test]
    fn point_in_polygon_inside() {
        let square = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(point_in_polygon(0.5, 0.5, &square));
    }

    #[test]
    fn point_in_polygon_outside() {
        let square = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(!point_in_polygon(1.5, 0.5, &square));
    }

    #[test]
    fn default_silhouette_is_closed() {
        let path = default_silhouette();
        assert!(!path.segments.is_empty());

        let first = path.segments.first().unwrap();
        let last = path.segments.last().unwrap();

        let eps = 1e-4;
        assert!(
            (last.p3.x - first.p0.x).abs() < eps,
            "path not closed in x: last.p3.x={}, first.p0.x={}",
            last.p3.x,
            first.p0.x
        );
        assert!(
            (last.p3.y - first.p0.y).abs() < eps,
            "path not closed in y: last.p3.y={}, first.p0.y={}",
            last.p3.y,
            first.p0.y
        );
    }

    #[test]
    fn rasterize_has_content() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);
        assert_eq!(mask.len(), 70);
        assert!(mask.iter().all(|row| row.len() == 40));

        let filled_count: usize = mask
            .iter()
            .flat_map(|row| row.iter())
            .filter(|&&v| v > 0.0)
            .count();
        assert!(filled_count > 0);
    }

    #[test]
    fn rasterize_center_is_filled() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);
        // Center of figure at normalized (0.5, 0.5) -> pixel (20, 35)
        let val = mask[35][20];
        assert!(val > 0.5, "center pixel should be filled, got {val}");
    }

    #[test]
    fn rasterize_shoulders_are_wide() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);
        // At shoulder height (~y=0.32 -> pixel row 22), the figure should
        // span at least 40% of the width (16+ pixels at width 40).
        let shoulder_row = &mask[22];
        let filled = shoulder_row.iter().filter(|&&v| v > 0.0).count();
        assert!(
            filled >= 14,
            "shoulders should be wide, got {filled} filled pixels"
        );
    }

    #[test]
    fn fill_gradient_top_bottom_differ() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);

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

    #[test]
    fn rasterize_zero_size() {
        let path = default_silhouette();
        assert!(rasterize(&path, 0, 0).is_empty());
        assert!(rasterize(&path, 10, 0).is_empty());
        assert!(rasterize(&path, 0, 10).is_empty());
    }
}

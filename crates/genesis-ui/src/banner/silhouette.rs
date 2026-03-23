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
/// space. The figure faces forward, centered horizontally at x=0.5.
pub fn default_silhouette() -> SilhouettePath {
    // Key landmark points (right side — left is mirrored).
    //
    // The figure occupies roughly the center 30% of width and center 75% of
    // height, leaving room for surrounding geometry.

    // Crown of head
    let crown = Point { x: 0.50, y: 0.08 };

    // Right side landmarks
    let head_r1 = Point { x: 0.56, y: 0.08 };
    let head_r2 = Point { x: 0.57, y: 0.12 };
    let head_r3 = Point { x: 0.55, y: 0.18 };
    let neck_r = Point { x: 0.52, y: 0.22 };
    let shoulder_r = Point { x: 0.62, y: 0.28 };
    let arm_r = Point { x: 0.64, y: 0.35 };
    let waist_r = Point { x: 0.57, y: 0.45 };
    let hip_r = Point { x: 0.62, y: 0.55 };
    let thigh_r = Point { x: 0.55, y: 0.72 };
    let dissolve_r = Point { x: 0.52, y: 0.82 };

    // Mirror helper
    let mirror = |p: Point| Point {
        x: 1.0 - p.x,
        y: p.y,
    };

    // Left side landmarks (mirrored)
    let head_l1 = mirror(head_r1);
    let head_l2 = mirror(head_r2);
    let head_l3 = mirror(head_r3);
    let neck_l = mirror(neck_r);
    let shoulder_l = mirror(shoulder_r);
    let arm_l = mirror(arm_r);
    let waist_l = mirror(waist_r);
    let hip_l = mirror(hip_r);
    let thigh_l = mirror(thigh_r);
    let dissolve_l = mirror(dissolve_r);

    // Bottom center — gentle curve connecting the two dissolve points
    let bottom_center = Point { x: 0.50, y: 0.84 };

    // Build segments going clockwise starting from crown.
    let segments = vec![
        // === RIGHT SIDE (top to bottom) ===
        // Crown -> right head curve
        CubicBezier {
            p0: crown,
            p1: Point { x: 0.53, y: 0.06 },
            p2: Point { x: 0.56, y: 0.06 },
            p3: head_r1,
        },
        // Right head upper -> right head lower
        CubicBezier {
            p0: head_r1,
            p1: head_r2,
            p2: Point { x: 0.57, y: 0.15 },
            p3: head_r3,
        },
        // Right head lower -> neck
        CubicBezier {
            p0: head_r3,
            p1: Point { x: 0.54, y: 0.20 },
            p2: Point { x: 0.53, y: 0.21 },
            p3: neck_r,
        },
        // Neck -> shoulder
        CubicBezier {
            p0: neck_r,
            p1: Point { x: 0.54, y: 0.23 },
            p2: Point { x: 0.59, y: 0.25 },
            p3: shoulder_r,
        },
        // Shoulder -> upper arm
        CubicBezier {
            p0: shoulder_r,
            p1: Point { x: 0.64, y: 0.30 },
            p2: Point { x: 0.65, y: 0.33 },
            p3: arm_r,
        },
        // Upper arm -> waist
        CubicBezier {
            p0: arm_r,
            p1: Point { x: 0.63, y: 0.38 },
            p2: Point { x: 0.59, y: 0.42 },
            p3: waist_r,
        },
        // Waist -> hip
        CubicBezier {
            p0: waist_r,
            p1: Point { x: 0.56, y: 0.48 },
            p2: Point { x: 0.63, y: 0.52 },
            p3: hip_r,
        },
        // Hip -> thigh
        CubicBezier {
            p0: hip_r,
            p1: Point { x: 0.62, y: 0.60 },
            p2: Point { x: 0.57, y: 0.68 },
            p3: thigh_r,
        },
        // Thigh -> dissolve
        CubicBezier {
            p0: thigh_r,
            p1: Point { x: 0.54, y: 0.75 },
            p2: Point { x: 0.53, y: 0.79 },
            p3: dissolve_r,
        },
        // === BOTTOM (right dissolve -> left dissolve) ===
        CubicBezier {
            p0: dissolve_r,
            p1: Point { x: 0.51, y: 0.84 },
            p2: Point { x: 0.50, y: 0.84 },
            p3: bottom_center,
        },
        CubicBezier {
            p0: bottom_center,
            p1: Point { x: 0.50, y: 0.84 },
            p2: Point { x: 0.49, y: 0.84 },
            p3: dissolve_l,
        },
        // === LEFT SIDE (bottom to top, mirrored) ===
        // Dissolve -> thigh
        CubicBezier {
            p0: dissolve_l,
            p1: Point { x: 0.47, y: 0.79 },
            p2: Point { x: 0.46, y: 0.75 },
            p3: thigh_l,
        },
        // Thigh -> hip
        CubicBezier {
            p0: thigh_l,
            p1: Point { x: 0.43, y: 0.68 },
            p2: Point { x: 0.38, y: 0.60 },
            p3: hip_l,
        },
        // Hip -> waist
        CubicBezier {
            p0: hip_l,
            p1: Point { x: 0.37, y: 0.52 },
            p2: Point { x: 0.44, y: 0.48 },
            p3: waist_l,
        },
        // Waist -> upper arm
        CubicBezier {
            p0: waist_l,
            p1: Point { x: 0.41, y: 0.42 },
            p2: Point { x: 0.37, y: 0.38 },
            p3: arm_l,
        },
        // Upper arm -> shoulder
        CubicBezier {
            p0: arm_l,
            p1: Point { x: 0.35, y: 0.33 },
            p2: Point { x: 0.36, y: 0.30 },
            p3: shoulder_l,
        },
        // Shoulder -> neck
        CubicBezier {
            p0: shoulder_l,
            p1: Point { x: 0.41, y: 0.25 },
            p2: Point { x: 0.46, y: 0.23 },
            p3: neck_l,
        },
        // Neck -> left head lower
        CubicBezier {
            p0: neck_l,
            p1: Point { x: 0.47, y: 0.21 },
            p2: Point { x: 0.46, y: 0.20 },
            p3: head_l3,
        },
        // Left head lower -> left head upper
        CubicBezier {
            p0: head_l3,
            p1: Point { x: 0.43, y: 0.15 },
            p2: head_l2,
            p3: head_l1,
        },
        // Left head upper -> crown (closing the path)
        CubicBezier {
            p0: head_l1,
            p1: Point { x: 0.44, y: 0.06 },
            p2: Point { x: 0.47, y: 0.06 },
            p3: crown,
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
    let mut vertices: Vec<(f32, f32)> = Vec::with_capacity(
        path.segments.len() * samples_per_segment,
    );

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
    let pixel_vertices: Vec<(f32, f32)> = vertices
        .iter()
        .map(|&(x, y)| (x * w, y * h))
        .collect();

    let mut mask = vec![vec![0.0f32; width as usize]; height as usize];

    for row in 0..height {
        for col in 0..width {
            let cx = col as f32 + 0.5;
            let cy = row as f32 + 0.5;

            if point_in_polygon(cx, cy, &pixel_vertices) {
                // Check if this is an edge pixel by testing sub-pixel corners.
                let offsets: [(f32, f32); 4] = [
                    (0.25, 0.25),
                    (0.75, 0.25),
                    (0.25, 0.75),
                    (0.75, 0.75),
                ];
                let inside_count = offsets
                    .iter()
                    .filter(|&&(dx, dy)| {
                        point_in_polygon(
                            col as f32 + dx,
                            row as f32 + dy,
                            &pixel_vertices,
                        )
                    })
                    .count();
                mask[row as usize][col as usize] = inside_count as f32 / 4.0;
            } else {
                // Check if any sub-pixel is inside (edge pixel from outside).
                let offsets: [(f32, f32); 4] = [
                    (0.25, 0.25),
                    (0.75, 0.25),
                    (0.25, 0.75),
                    (0.75, 0.75),
                ];
                let inside_count = offsets
                    .iter()
                    .filter(|&&(dx, dy)| {
                        point_in_polygon(
                            col as f32 + dx,
                            row as f32 + dy,
                            &pixel_vertices,
                        )
                    })
                    .count();
                if inside_count > 0 {
                    mask[row as usize][col as usize] = inside_count as f32 / 4.0;
                }
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
///
/// Casts a ray from `(px, py)` to the right and counts edge crossings. An odd
/// count means the point is inside.
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

        if ((yi > py) != (yj > py))
            && (px < (xj - xi) * (py - yi) / (yj - yi) + xi)
        {
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
        // For B(t) with P0=(0,0), P1=(0,1), P2=(1,0), P3=(1,1):
        // B(0.5) = (1/8)(0,0) + (3/8)(0,1) + (3/8)(1,0) + (1/8)(1,1)
        //        = (0 + 0 + 3/8 + 1/8, 0 + 3/8 + 0 + 1/8) = (0.5, 0.5)
        let curve = CubicBezier {
            p0: Point { x: 0.0, y: 0.0 },
            p1: Point { x: 0.0, y: 1.0 },
            p2: Point { x: 1.0, y: 0.0 },
            p3: Point { x: 1.0, y: 1.0 },
        };

        let mid = evaluate_bezier(&curve, 0.5);
        assert!(
            (mid.x - 0.5).abs() < 1e-5,
            "expected x ~0.5, got {}",
            mid.x
        );
        assert!(
            (mid.y - 0.5).abs() < 1e-5,
            "expected y ~0.5, got {}",
            mid.y
        );
    }

    #[test]
    fn point_in_polygon_inside() {
        // Unit square: (0,0), (1,0), (1,1), (0,1)
        let square = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(point_in_polygon(0.5, 0.5, &square));
        assert!(point_in_polygon(0.1, 0.1, &square));
        assert!(point_in_polygon(0.9, 0.9, &square));
    }

    #[test]
    fn point_in_polygon_outside() {
        let square = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        assert!(!point_in_polygon(1.5, 0.5, &square));
        assert!(!point_in_polygon(-0.5, 0.5, &square));
        assert!(!point_in_polygon(0.5, 1.5, &square));
        assert!(!point_in_polygon(0.5, -0.5, &square));
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
        assert!(
            filled_count > 0,
            "expected some filled pixels, got none"
        );
    }

    #[test]
    fn rasterize_center_is_filled() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);

        // The figure is centered at x=0.5, and at y ~0.5 the torso should
        // be solidly filled. Pixel (20, 35) maps to normalized (0.5, 0.5).
        let val = mask[35][20];
        assert!(
            val > 0.5,
            "expected center pixel to be filled (~1.0), got {}",
            val
        );
    }

    #[test]
    fn fill_gradient_top_bottom_differ() {
        let path = default_silhouette();
        let mask = rasterize(&path, 40, 70);

        let top_color = RgbColor::new(200, 100, 255);
        let bottom_color = RgbColor::new(50, 150, 200);
        let filled = fill_gradient(&mask, 40, 70, top_color, bottom_color);

        // Find a filled pixel near the top and one near the bottom.
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

        assert_ne!(
            top_pixel, bottom_pixel,
            "top and bottom gradient colors should differ"
        );
    }

    #[test]
    fn rasterize_zero_size() {
        let path = default_silhouette();

        let mask = rasterize(&path, 0, 0);
        assert!(mask.is_empty());

        let mask = rasterize(&path, 10, 0);
        assert!(mask.is_empty());

        let mask = rasterize(&path, 0, 10);
        assert!(mask.is_empty());
    }
}

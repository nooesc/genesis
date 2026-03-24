//! BrailleCanvas widget with a pluggable pattern system.
//!
//! Uses ratatui's built-in `Canvas` widget with `Marker::Braille` (2x4
//! sub-pixel resolution per cell) to render lightweight animations in
//! the TUI: waveforms, particles, Lissajous curves, and flatlines.

use std::f64::consts::TAU;
use std::time::Duration;

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Color,
    symbols::Marker,
    widgets::{
        canvas::{Canvas, Line as CanvasLine, Points},
        Widget,
    },
};

// ── Palette ─────────────────────────────────────────────────────────────────

/// Eve lavender accent for active dots.
const EVE_LAVENDER: Color = crate::history::rgb(genesis_ui::colors::EVE_LAVENDER);
/// Dim grey for flatline / inactive (slightly dimmer than UI_DIM).
const DIM_GREY: Color = Color::Rgb(98, 98, 98);

// ── Pattern ─────────────────────────────────────────────────────────────────

/// An animatable pattern that can be rendered on a braille canvas.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// A sine waveform with advancing phase.
    Waveform { phase: f64, frequency: f64 },
    /// A cloud of particles with position and velocity, gentle gravity, wrap-around.
    Particles {
        points: Vec<(f64, f64, f64, f64)>, // x, y, vx, vy
    },
    /// A Lissajous parametric curve: x = sin(a*t + delta), y = sin(b*t).
    Lissajous { t: f64, a: f64, b: f64, delta: f64 },
    /// A static horizontal line at mid-height.
    Flatline,
    /// Matrix-style falling rain columns with varying speeds and offsets.
    MatrixRain {
        /// Per-column state: (y_head, speed, length, brightness).
        columns: Vec<(f64, f64, f64, f64)>,
        /// Accumulated time since last position update (throttles to ~7fps).
        accum: f64,
    },
}

impl Pattern {
    /// Advance the pattern simulation by `dt`.
    pub fn tick(&mut self, dt: Duration) {
        let secs = dt.as_secs_f64();
        match self {
            Pattern::Waveform { phase, frequency } => {
                *phase += secs * *frequency * TAU;
                // Keep phase bounded to avoid precision loss.
                if *phase > TAU * 100.0 {
                    *phase -= TAU * 100.0;
                }
            }
            Pattern::Particles { points } => {
                const GRAVITY: f64 = -0.3;
                for (x, y, vx, vy) in points.iter_mut() {
                    *vy += GRAVITY * secs;
                    *x += *vx * secs;
                    *y += *vy * secs;
                    // Wrap around [0, 1] bounds (handles large dt jumps).
                    *x = x.rem_euclid(1.0);
                    if *y < 0.0 {
                        *y = 0.0;
                        *vy = vy.abs() * 0.6; // bounce
                    } else if *y > 1.0 {
                        *y = 1.0;
                        *vy = -(vy.abs() * 0.6);
                    }
                }
            }
            Pattern::Lissajous { t, .. } => {
                *t += secs * 2.0;
                if *t > TAU * 100.0 {
                    *t -= TAU * 100.0;
                }
            }
            Pattern::Flatline => {}
            Pattern::MatrixRain { columns, accum } => {
                const TICK_INTERVAL: f64 = 0.15; // update positions ~7 times/sec
                *accum += secs;
                if *accum >= TICK_INTERVAL {
                    let steps = *accum / TICK_INTERVAL;
                    let advance = TICK_INTERVAL * steps.floor();
                    *accum -= advance;
                    for (y_head, speed, _, _) in columns.iter_mut() {
                        *y_head += advance * *speed;
                        if *y_head > 1.8 {
                            *y_head -= 2.0;
                        }
                    }
                }
            }
        }
    }

    /// Create a dense two-layer matrix rain pattern.
    ///
    /// Background layer: many slow, dim columns.
    /// Foreground layer: fewer fast, bright columns with long trails.
    pub fn matrix_rain(density: usize) -> Self {
        let mut seed: u64 = 7;
        let mut next = || -> f64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64) / (u32::MAX as f64)
        };

        let mut columns = Vec::with_capacity(density);

        // Background layer — slow, dim, many columns
        let bg_count = density * 2 / 3;
        for _ in 0..bg_count {
            let y_head = next() * 2.2 - 0.6;
            let speed = 0.04 + next() * 0.08;
            let length = 0.15 + next() * 0.30;
            let brightness = 0.15 + next() * 0.25; // dim
            columns.push((y_head, speed, length, brightness));
        }

        // Foreground layer — fast, bright, long trails
        let fg_count = density - bg_count;
        for _ in 0..fg_count {
            let y_head = next() * 2.2 - 0.6;
            let speed = 0.12 + next() * 0.25;
            let length = 0.30 + next() * 0.50;
            let brightness = 0.6 + next() * 0.4; // bright
            columns.push((y_head, speed, length, brightness));
        }

        Pattern::MatrixRain { columns, accum: 0.0 }
    }

    /// Create a default set of particles for the welcome screen.
    pub fn default_particles(count: usize) -> Self {
        // Deterministic "random" using a simple linear congruential generator.
        let mut seed: u64 = 42;
        let mut next = || -> f64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64) / (u32::MAX as f64)
        };

        let points = (0..count)
            .map(|_| {
                let x = next();
                let y = next();
                let vx = (next() - 0.5) * 0.3;
                let vy = (next() - 0.5) * 0.3;
                (x, y, vx, vy)
            })
            .collect();

        Pattern::Particles { points }
    }
}

// ── BrailleCanvas Widget ────────────────────────────────────────────────────

/// A reusable widget that renders a `Pattern` onto a ratatui `Canvas`
/// with `Marker::Braille`.
pub struct BrailleCanvas<'a> {
    pattern: &'a Pattern,
}

impl<'a> BrailleCanvas<'a> {
    /// Create a new braille canvas widget for the given pattern.
    pub const fn new(pattern: &'a Pattern) -> Self {
        Self { pattern }
    }

    /// Render into a buffer at the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let w = f64::from(area.width);
        let h = f64::from(area.height);

        match self.pattern {
            Pattern::Waveform { phase, frequency } => {
                Self::render_waveform(*phase, *frequency, w, h, area, buf);
            }
            Pattern::Particles { points } => {
                Self::render_particles(points, w, h, area, buf);
            }
            Pattern::Lissajous { t, a, b, delta } => {
                Self::render_lissajous((*t, *a, *b, *delta), w, h, area, buf);
            }
            Pattern::Flatline => {
                Self::render_flatline(w, h, area, buf);
            }
            Pattern::MatrixRain { columns, .. } => {
                Self::render_matrix_rain(columns, w, h, area, buf);
            }
        }
    }

    fn render_waveform(phase: f64, frequency: f64, w: f64, h: f64, area: Rect, buf: &mut Buffer) {
        // Generate sine wave sample points.
        let num_points = (w * 2.0) as usize; // 2x resolution
        let coords: Vec<(f64, f64)> = (0..num_points.max(2))
            .map(|i| {
                let x = (i as f64) / (num_points as f64 - 1.0).max(1.0) * w;
                let t = (i as f64) / (num_points as f64 - 1.0).max(1.0) * TAU * frequency;
                let y = (t + phase).sin() * (h / 2.0 - 0.5) + h / 2.0;
                (x, y)
            })
            .collect();

        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, w])
            .y_bounds([0.0, h])
            .paint(|ctx| {
                ctx.draw(&Points {
                    coords: &coords,
                    color: EVE_LAVENDER,
                });
            });
        Widget::render(canvas, area, buf);
    }

    fn render_particles(
        points: &[(f64, f64, f64, f64)],
        w: f64,
        h: f64,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let coords: Vec<(f64, f64)> = points.iter().map(|(x, y, _, _)| (x * w, y * h)).collect();

        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, w])
            .y_bounds([0.0, h])
            .paint(|ctx| {
                ctx.draw(&Points {
                    coords: &coords,
                    color: EVE_LAVENDER,
                });
            });
        Widget::render(canvas, area, buf);
    }

    fn render_lissajous(
        params: (f64, f64, f64, f64), // (t, a, b, delta)
        w: f64,
        h: f64,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let (t, a, b, delta) = params;
        // Trace the curve as a series of points over a sweep of the parameter.
        let num_points = 200usize;
        let coords: Vec<(f64, f64)> = (0..num_points)
            .map(|i| {
                let param = t - (i as f64) * 0.05;
                let x = (a * param + delta).sin() * (w / 2.0 - 1.0) + w / 2.0;
                let y = (b * param).sin() * (h / 2.0 - 0.5) + h / 2.0;
                (x, y)
            })
            .collect();

        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, w])
            .y_bounds([0.0, h])
            .paint(|ctx| {
                ctx.draw(&Points {
                    coords: &coords,
                    color: EVE_LAVENDER,
                });
            });
        Widget::render(canvas, area, buf);
    }

    /// Direct-to-buffer matrix rain renderer with full braille glyphs.
    ///
    /// Each column generates a trail of random braille characters (not just
    /// single dots). Head cells are bright with dense glyphs; tail cells
    /// fade with sparser patterns. Creates an alien data-stream aesthetic.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn render_matrix_rain(
        columns: &[(f64, f64, f64, f64)],
        _w: f64,
        _h: f64,
        area: Rect,
        buf: &mut Buffer,
    ) {
        const BRIGHT_HEAD: Color = Color::Rgb(210, 200, 235);
        const BRIGHT_TRAIL: Color = Color::Rgb(150, 140, 185);
        const MID_TRAIL: Color = Color::Rgb(100, 92, 130);
        const DIM_TRAIL: Color = Color::Rgb(55, 50, 70);

        // Glyph tables — denser patterns for heads, sparser for tails.
        // Each u8 is the braille dot bitmask (0x00-0xFF).
        const DENSE_GLYPHS: [u8; 16] = [
            0xFF, 0xF7, 0x7F, 0xBF, 0xFE, 0xDB, 0xED, 0xBE,
            0x77, 0xEE, 0xD7, 0x7D, 0xBB, 0xEB, 0xF5, 0xAF,
        ];
        const MID_GLYPHS: [u8; 16] = [
            0x49, 0x92, 0x24, 0x6C, 0x36, 0x5A, 0xA5, 0xC3,
            0x1E, 0x78, 0x4B, 0xD2, 0x69, 0x96, 0x33, 0xCC,
        ];
        const SPARSE_GLYPHS: [u8; 16] = [
            0x01, 0x08, 0x02, 0x10, 0x04, 0x20, 0x40, 0x80,
            0x09, 0x12, 0x24, 0x48, 0x03, 0x18, 0x06, 0x30,
        ];

        let num_cols = columns.len().max(1);
        let area_w = area.width;
        let area_h = area.height;

        if area_w == 0 || area_h == 0 {
            return;
        }

        // Deterministic hash for glyph selection (varies by position).
        let glyph_hash = |col: u16, row: u16, salt: u16| -> usize {
            let v = (col as u32)
                .wrapping_mul(2654435761)
                .wrapping_add(row as u32)
                .wrapping_mul(2246822519)
                .wrapping_add(salt as u32);
            (v >> 16) as usize
        };

        for (i, &(y_head, _speed, length, brightness)) in columns.iter().enumerate() {
            // Map column to a terminal cell column.
            let cell_col = ((i as f64 + 0.5) / num_cols as f64 * area_w as f64) as u16;
            if cell_col >= area_w {
                continue;
            }

            // How many terminal rows this trail covers.
            let trail_cells = (length * area_h as f64) as u16;
            if trail_cells == 0 {
                continue;
            }

            // Head row in terminal cells.
            let head_row = (y_head * area_h as f64) as i32;

            for d in 0..trail_cells {
                let row = head_row - d as i32;
                if row < 0 || row >= area_h as i32 {
                    continue;
                }
                let row = row as u16;

                let frac = d as f64 / trail_cells as f64;
                let salt = i as u16;
                let h = glyph_hash(cell_col, row, salt);

                // Select glyph and color based on position in trail.
                let (glyph_bits, color) = if d == 0 && brightness > 0.5 {
                    // Bright head — dense glyph, white-ish
                    (DENSE_GLYPHS[h % DENSE_GLYPHS.len()], BRIGHT_HEAD)
                } else if frac < 0.2 {
                    // Near head — dense, bright
                    (DENSE_GLYPHS[h % DENSE_GLYPHS.len()], BRIGHT_TRAIL)
                } else if frac < 0.5 {
                    // Mid trail
                    (MID_GLYPHS[h % MID_GLYPHS.len()], MID_TRAIL)
                } else if frac < 0.8 {
                    // Fading
                    (SPARSE_GLYPHS[h % SPARSE_GLYPHS.len()], DIM_TRAIL)
                } else {
                    // Tail — very sparse
                    let sparse = SPARSE_GLYPHS[h % SPARSE_GLYPHS.len()];
                    // Randomly drop some tail cells entirely.
                    if h % 3 == 0 {
                        continue;
                    }
                    (sparse, DIM_TRAIL)
                };

                if glyph_bits == 0 {
                    continue;
                }

                let x = area.x + cell_col;
                let y = area.y + row;

                if let Some(cell) = buf.cell_mut((x, y)) {
                    // Merge with existing braille character if present.
                    let existing = cell.symbol().chars().next().unwrap_or(' ');
                    let existing_bits = if ('\u{2800}'..='\u{28FF}').contains(&existing) {
                        (existing as u32 - 0x2800) as u8
                    } else {
                        0
                    };
                    let merged = existing_bits | glyph_bits;
                    let ch = char::from_u32(0x2800 + u32::from(merged)).unwrap_or(' ');
                    cell.set_char(ch);
                    cell.set_fg(color);
                }
            }
        }
    }

    fn render_flatline(w: f64, h: f64, area: Rect, buf: &mut Buffer) {
        let mid_y = h / 2.0;
        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, w])
            .y_bounds([0.0, h])
            .paint(|ctx| {
                ctx.draw(&CanvasLine {
                    x1: 0.0,
                    y1: mid_y,
                    x2: w,
                    y2: mid_y,
                    color: DIM_GREY,
                });
            });
        Widget::render(canvas, area, buf);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_tick_advances_phase() {
        let mut pat = Pattern::Waveform {
            phase: 0.0,
            frequency: 1.0,
        };
        pat.tick(Duration::from_millis(100));
        if let Pattern::Waveform { phase, .. } = pat {
            assert!(phase > 0.0, "phase should advance: got {phase}");
        } else {
            panic!("expected Waveform");
        }
    }

    #[test]
    fn particles_tick_moves_positions() {
        let mut pat = Pattern::Particles {
            points: vec![(0.5, 0.5, 0.1, 0.1)],
        };
        pat.tick(Duration::from_millis(100));
        if let Pattern::Particles { points } = &pat {
            let (x, y, _, _) = points[0];
            assert!(
                (x - 0.5).abs() > 1e-6 || (y - 0.5).abs() > 1e-6,
                "particle should have moved from center"
            );
        } else {
            panic!("expected Particles");
        }
    }

    #[test]
    fn lissajous_tick_advances_t() {
        let mut pat = Pattern::Lissajous {
            t: 0.0,
            a: 3.0,
            b: 2.0,
            delta: std::f64::consts::FRAC_PI_2,
        };
        pat.tick(Duration::from_millis(100));
        if let Pattern::Lissajous { t, .. } = pat {
            assert!(t > 0.0, "t should advance: got {t}");
        } else {
            panic!("expected Lissajous");
        }
    }

    #[test]
    fn flatline_tick_is_noop() {
        let mut pat = Pattern::Flatline;
        pat.tick(Duration::from_secs(10));
        assert!(matches!(pat, Pattern::Flatline));
    }

    #[test]
    fn flatline_renders_braille_characters() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        let pat = Pattern::Flatline;
        BrailleCanvas::new(&pat).render(area, &mut buf);

        // The middle row should have non-space braille characters.
        let mid_row = 1u16;
        let has_braille = (0..10).any(|x| {
            let sym = buf
                .cell((x, mid_row))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default();
            sym != " "
        });
        assert!(
            has_braille,
            "flatline should render some braille dots in the middle row"
        );
    }

    #[test]
    fn default_particles_creates_correct_count() {
        if let Pattern::Particles { points } = Pattern::default_particles(15) {
            assert_eq!(points.len(), 15);
            // All positions should be in [0, 1] range.
            for (x, y, _, _) in &points {
                assert!((0.0..=1.0).contains(x), "x={x} out of range");
                assert!((0.0..=1.0).contains(y), "y={y} out of range");
            }
        } else {
            panic!("expected Particles");
        }
    }

    #[test]
    fn particles_wrap_around() {
        let mut pat = Pattern::Particles {
            points: vec![(0.01, 0.5, -0.5, 0.0)], // moving left fast
        };
        pat.tick(Duration::from_millis(100)); // x moves -0.05 → wraps
        if let Pattern::Particles { points } = &pat {
            let (x, _, _, _) = points[0];
            assert!(
                (0.0..=1.0).contains(&x),
                "particle should wrap around: got x={x}"
            );
        }
    }

    #[test]
    fn zero_area_does_not_panic() {
        let pat = Pattern::Waveform {
            phase: 0.0,
            frequency: 1.0,
        };
        let area = Rect::new(0, 0, 0, 0);
        let mut buf = Buffer::empty(area);
        BrailleCanvas::new(&pat).render(area, &mut buf);
    }

    #[test]
    fn waveform_renders_without_panic() {
        let area = Rect::new(0, 0, 20, 5);
        let mut buf = Buffer::empty(area);
        let pat = Pattern::Waveform {
            phase: 1.5,
            frequency: 2.0,
        };
        BrailleCanvas::new(&pat).render(area, &mut buf);

        // Check that at least some cells have non-space content.
        let has_content = buf.content.iter().any(|cell| cell.symbol() != " ");
        assert!(has_content, "waveform should produce visible braille dots");
    }

    #[test]
    fn lissajous_renders_without_panic() {
        let area = Rect::new(0, 0, 15, 5);
        let mut buf = Buffer::empty(area);
        let pat = Pattern::Lissajous {
            t: 3.0,
            a: 3.0,
            b: 2.0,
            delta: std::f64::consts::FRAC_PI_2,
        };
        BrailleCanvas::new(&pat).render(area, &mut buf);
        let has_content = buf.content.iter().any(|cell| cell.symbol() != " ");
        assert!(has_content, "lissajous should produce visible braille dots");
    }
}

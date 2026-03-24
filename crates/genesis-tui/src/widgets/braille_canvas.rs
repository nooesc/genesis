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

    /// Create a matrix rain pattern with the given number of columns.
    pub fn matrix_rain(num_columns: usize) -> Self {
        let mut seed: u64 = 7;
        let mut next = || -> f64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((seed >> 33) as f64) / (u32::MAX as f64)
        };

        let columns = (0..num_columns)
            .map(|_| {
                let y_head = next() * 1.8 - 0.4;
                let speed = 0.08 + next() * 0.15; // slower for subtlety
                let length = 0.10 + next() * 0.20; // shorter trails
                let brightness = 0.4 + next() * 0.6;
                (y_head, speed, length, brightness)
            })
            .collect();

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

    /// Direct-to-buffer matrix rain renderer. Bypasses the Canvas widget
    /// entirely — no internal bitmap allocation, just writes braille chars
    /// directly to the cells that need them. O(columns × dots) instead of
    /// O(area_width × area_height).
    fn render_matrix_rain(
        columns: &[(f64, f64, f64, f64)],
        _w: f64,
        _h: f64,
        area: Rect,
        buf: &mut Buffer,
    ) {
        // Braille dot layout per cell (2 wide × 4 tall):
        //   1 4      bit 0  bit 3
        //   2 5      bit 1  bit 4
        //   3 6      bit 2  bit 5
        //   7 8      bit 6  bit 7
        const LEFT_DOTS: [u8; 4] = [0x01, 0x02, 0x04, 0x40];
        const RIGHT_DOTS: [u8; 4] = [0x08, 0x10, 0x20, 0x80];
        const DOTS_PER_TRAIL: usize = 5;
        const DIM_LAVENDER: Color = Color::Rgb(60, 55, 72);

        let num_cols = columns.len().max(1);
        let area_w = area.width as f64;
        let area_h = area.height as f64;

        // Track which cells have been touched and their accumulated braille bits.
        // Key: (col, row), Value: (bits, is_bright)
        let mut cell_bits: std::collections::HashMap<(u16, u16), (u8, bool)> =
            std::collections::HashMap::new();

        for (i, &(y_head, _speed, length, brightness)) in columns.iter().enumerate() {
            // Map column index to pixel x in the area.
            let norm_x = (i as f64 + 0.5) / num_cols as f64;
            let px_x = norm_x * area_w * 2.0; // braille has 2 sub-pixels per cell width
            let cell_col = (px_x / 2.0) as u16;
            let sub_x = (px_x as u16) % 2; // 0 = left column, 1 = right column
            let dot_col = if sub_x == 0 { &LEFT_DOTS } else { &RIGHT_DOTS };

            if cell_col >= area.width {
                continue;
            }

            for d in 0..DOTS_PER_TRAIL {
                let frac = d as f64 / DOTS_PER_TRAIL as f64;
                let norm_y = y_head - frac * length;

                if norm_y < 0.0 || norm_y >= 1.0 {
                    continue;
                }

                // Map to sub-pixel y (4 sub-pixels per cell height).
                let px_y = norm_y * area_h * 4.0;
                let cell_row = (px_y / 4.0) as u16;
                let sub_y = (px_y as usize) % 4;

                if cell_row >= area.height {
                    continue;
                }

                let is_bright = d == 0 && brightness > 0.6;
                let entry = cell_bits.entry((cell_col, cell_row)).or_insert((0, false));
                entry.0 |= dot_col[sub_y];
                if is_bright {
                    entry.1 = true;
                }
            }
        }

        // Write accumulated braille characters to the buffer.
        for (&(col, row), &(bits, is_bright)) in &cell_bits {
            if bits == 0 {
                continue;
            }

            let x = area.x + col;
            let y = area.y + row;

            if x >= area.x + area.width || y >= area.y + area.height {
                continue;
            }

            let ch = char::from_u32(0x2800 + u32::from(bits)).unwrap_or(' ');
            let color = if is_bright { EVE_LAVENDER } else { DIM_LAVENDER };

            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_char(ch);
                cell.set_fg(color);
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

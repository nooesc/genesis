//! DECSTBM scroll region insertion — pushes completed conversation turns into
//! terminal scrollback above the viewport.
//!
//! Uses ANSI Set Top and Bottom Margins (DECSTBM, `ESC [ top ; bottom r`) to
//! confine scrolling to the region above the viewport, then emits `\r\n` to
//! push existing content upward while writing the new history lines.

use std::fmt;
use std::io::{self, Write};
use std::ops::Range;

use crossterm::Command;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::{
    Attribute as CAttribute, Colors, Print, SetAttribute, SetBackgroundColor, SetColors,
    SetForegroundColor,
};
use ratatui::style::{Color, Modifier};
use ratatui::text::Span;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A pre-rendered line ready for insertion into scrollback.
#[derive(Debug, Clone, Default)]
pub struct RenderedLine {
    pub spans: Vec<RenderedSpan>,
}

/// A styled text span within a rendered line.
#[derive(Debug, Clone)]
pub struct RenderedSpan {
    pub text: String,
    pub fg: Option<(u8, u8, u8)>,
    pub bg: Option<(u8, u8, u8)>,
    pub bold: bool,
}

// ---------------------------------------------------------------------------
// ANSI commands
// ---------------------------------------------------------------------------

/// DECSTBM — Set Scroll Region. `ESC [ top ; bottom r` (1-based rows).
///
/// Restricts terminal scrolling to the lines in `range`. Both endpoints are
/// 1-based row numbers inclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

/// Reset scroll region to full screen. `ESC [ r`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Core insertion function
// ---------------------------------------------------------------------------

/// Insert rendered lines into terminal scrollback above the viewport.
///
/// # Algorithm
///
/// 1. Set scroll region to the area above the viewport
///    (rows `1..viewport_top`, 1-based).
/// 2. Move cursor to the bottom of the scroll region
///    (0-based row `viewport_top - 1`).
/// 3. Emit `\r\n` for each line — this pushes existing content upward.
/// 4. Write line content with styling (fg/bg/bold per span).
/// 5. Reset scroll region to full screen.
///
/// Returns the number of lines actually inserted (0 if `viewport_top == 0` or
/// `lines` is empty, since there is no room above the viewport).
pub fn insert_history_lines<W: Write>(
    writer: &mut W,
    viewport_top: u16,
    lines: &[RenderedLine],
) -> io::Result<u16> {
    if viewport_top == 0 || lines.is_empty() {
        return Ok(0);
    }

    // Set scroll region to rows 1..viewport_top (1-based inclusive).
    queue!(writer, SetScrollRegion(1..viewport_top))?;

    // Move cursor to the last row of the scroll region (0-based).
    queue!(writer, MoveTo(0, viewport_top - 1))?;

    for line in lines {
        // Scroll up one row and move to column 0.
        queue!(writer, Print("\r\n"))?;

        // Write styled spans.
        write_rendered_spans(writer, &line.spans)?;
    }

    // Reset scroll region to full screen.
    queue!(writer, ResetScrollRegion)?;

    Ok(lines.len() as u16)
}

// ---------------------------------------------------------------------------
// Span writer
// ---------------------------------------------------------------------------

/// Write a slice of `RenderedSpan`s with ANSI color/attribute escapes.
fn write_rendered_spans<W: Write>(writer: &mut W, spans: &[RenderedSpan]) -> io::Result<()> {
    for span in spans {
        // Apply bold if needed.
        if span.bold {
            queue!(writer, SetAttribute(CAttribute::Bold))?;
        }

        // Apply fg/bg colors.
        let fg = span
            .fg
            .map(|(r, g, b)| crossterm::style::Color::Rgb { r, g, b })
            .unwrap_or(crossterm::style::Color::Reset);
        let bg = span
            .bg
            .map(|(r, g, b)| crossterm::style::Color::Rgb { r, g, b })
            .unwrap_or(crossterm::style::Color::Reset);
        queue!(writer, SetColors(Colors::new(fg, bg)))?;

        queue!(writer, Print(&span.text))?;

        // Reset attributes after each span so they don't bleed into the next.
        if span.bold {
            queue!(writer, SetAttribute(CAttribute::NormalIntensity))?;
        }
    }

    // Reset all styles at end of line.
    queue!(
        writer,
        SetForegroundColor(crossterm::style::Color::Reset),
        SetBackgroundColor(crossterm::style::Color::Reset),
        SetAttribute(CAttribute::Reset),
    )?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Converter: ratatui Lines → RenderedLines
// ---------------------------------------------------------------------------

/// Convert ratatui [`Line`]s to [`RenderedLine`]s for scrollback insertion.
///
/// Extracts RGB fg/bg colors and bold modifier from each [`Span`].  Named
/// colors (e.g. `Color::Red`) and indexed colors are mapped to their
/// approximate RGB values via [`color_to_rgb`].
pub fn lines_to_rendered(lines: &[ratatui::text::Line<'_>]) -> Vec<RenderedLine> {
    lines.iter().map(line_to_rendered).collect()
}

fn line_to_rendered(line: &ratatui::text::Line<'_>) -> RenderedLine {
    let spans = line
        .spans
        .iter()
        .map(|span| span_to_rendered(span, line.style))
        .collect();
    RenderedLine { spans }
}

fn span_to_rendered(span: &Span<'_>, line_style: ratatui::style::Style) -> RenderedSpan {
    // Merge the line-level style into the span style (span overrides line).
    let style = line_style.patch(span.style);

    let fg = style.fg.and_then(color_to_rgb);
    let bg = style.bg.and_then(color_to_rgb);

    let mut modifier = Modifier::empty();
    modifier.insert(style.add_modifier);
    modifier.remove(style.sub_modifier);
    let bold = modifier.contains(Modifier::BOLD);

    RenderedSpan {
        text: span.content.to_string(),
        fg,
        bg,
        bold,
    }
}

/// Map a ratatui [`Color`] to an RGB triple, returning `None` for
/// `Color::Reset` and `Color::default()`.
fn color_to_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Reset => None,
        // Named colors: approximate ANSI defaults.
        Color::Black => Some((0, 0, 0)),
        Color::Red => Some((128, 0, 0)),
        Color::Green => Some((0, 128, 0)),
        Color::Yellow => Some((128, 128, 0)),
        Color::Blue => Some((0, 0, 128)),
        Color::Magenta => Some((128, 0, 128)),
        Color::Cyan => Some((0, 128, 128)),
        Color::Gray => Some((192, 192, 192)),
        Color::DarkGray => Some((128, 128, 128)),
        Color::LightRed => Some((255, 0, 0)),
        Color::LightGreen => Some((0, 255, 0)),
        Color::LightYellow => Some((255, 255, 0)),
        Color::LightBlue => Some((0, 0, 255)),
        Color::LightMagenta => Some((255, 0, 255)),
        Color::LightCyan => Some((0, 255, 255)),
        Color::White => Some((255, 255, 255)),
        // Indexed 256-color: return None for simplicity.
        Color::Indexed(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Stylize as _;
    use ratatui::text::Line;

    // -- ANSI format tests ---------------------------------------------------

    #[test]
    fn set_scroll_region_ansi_format() {
        let cmd = SetScrollRegion(1..10);
        let mut buf = String::new();
        cmd.write_ansi(&mut buf).unwrap();
        assert_eq!(buf, "\x1b[1;10r");
    }

    #[test]
    fn reset_scroll_region_ansi_format() {
        let cmd = ResetScrollRegion;
        let mut buf = String::new();
        cmd.write_ansi(&mut buf).unwrap();
        assert_eq!(buf, "\x1b[r");
    }

    // -- Edge-case guards ----------------------------------------------------

    #[test]
    fn empty_lines_returns_zero() {
        let mut buf = Vec::new();
        let count = insert_history_lines(&mut buf, 10, &[]).unwrap();
        assert_eq!(count, 0);
        // Nothing should have been written.
        assert!(buf.is_empty());
    }

    #[test]
    fn zero_viewport_top_returns_zero() {
        let lines = vec![RenderedLine {
            spans: vec![RenderedSpan {
                text: "hello".into(),
                fg: None,
                bg: None,
                bold: false,
            }],
        }];
        let mut buf = Vec::new();
        let count = insert_history_lines(&mut buf, 0, &lines).unwrap();
        assert_eq!(count, 0);
        assert!(buf.is_empty());
    }

    // -- Insertion output ----------------------------------------------------

    #[test]
    fn single_line_returns_one() {
        let lines = vec![RenderedLine {
            spans: vec![RenderedSpan {
                text: "hello".into(),
                fg: None,
                bg: None,
                bold: false,
            }],
        }];
        let mut buf = Vec::new();
        let count = insert_history_lines(&mut buf, 10, &lines).unwrap();
        assert_eq!(count, 1);

        let output = String::from_utf8(buf).expect("valid utf-8");
        // Must contain the SetScrollRegion sequence.
        assert!(
            output.contains("\x1b[1;10r"),
            "expected DECSTBM set scroll region in output: {output:?}"
        );
        // Must contain ResetScrollRegion at end.
        assert!(
            output.contains("\x1b[r"),
            "expected DECSTBM reset in output: {output:?}"
        );
        // Must contain the text.
        assert!(
            output.contains("hello"),
            "expected line text in output: {output:?}"
        );
    }

    #[test]
    fn multiple_lines_returns_correct_count() {
        let make_line = |s: &str| RenderedLine {
            spans: vec![RenderedSpan {
                text: s.into(),
                fg: None,
                bg: None,
                bold: false,
            }],
        };
        let lines = vec![make_line("line1"), make_line("line2"), make_line("line3")];
        let mut buf = Vec::new();
        let count = insert_history_lines(&mut buf, 5, &lines).unwrap();
        assert_eq!(count, 3);
    }

    // -- Converter tests -----------------------------------------------------

    #[test]
    fn lines_to_rendered_extracts_rgb() {
        let line = Line::from(vec![
            ratatui::text::Span::styled(
                "hello",
                ratatui::style::Style::default().fg(Color::Rgb(1, 2, 3)),
            ),
        ]);
        let rendered = lines_to_rendered(&[line]);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].spans.len(), 1);
        assert_eq!(rendered[0].spans[0].fg, Some((1, 2, 3)));
        assert_eq!(rendered[0].spans[0].text, "hello");
    }

    #[test]
    fn lines_to_rendered_extracts_bold() {
        let line = Line::from(vec!["bold text".bold()]);
        let rendered = lines_to_rendered(&[line]);
        assert!(rendered[0].spans[0].bold, "expected bold flag to be set");
    }

    #[test]
    fn lines_to_rendered_no_color_gives_none() {
        let line = Line::from("plain text");
        let rendered = lines_to_rendered(&[line]);
        assert_eq!(rendered[0].spans[0].fg, None);
        assert_eq!(rendered[0].spans[0].bg, None);
        assert!(!rendered[0].spans[0].bold);
    }

    #[test]
    fn lines_to_rendered_inherits_line_style() {
        // Line-level green fg should propagate to a span without explicit color.
        let mut line = Line::from(vec![ratatui::text::Span::raw("text")]);
        line = line.style(Color::Green);
        let rendered = lines_to_rendered(&[line]);
        assert_eq!(
            rendered[0].spans[0].fg,
            color_to_rgb(Color::Green),
            "span should inherit line-level green color"
        );
    }

    #[test]
    fn color_to_rgb_reset_is_none() {
        assert_eq!(color_to_rgb(Color::Reset), None);
    }

    #[test]
    fn color_to_rgb_named_returns_some() {
        assert!(color_to_rgb(Color::Red).is_some());
        assert!(color_to_rgb(Color::White).is_some());
    }
}

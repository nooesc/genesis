//! Welcome screen widget with terminal-native portrait art.
//!
//! Displays a bordered welcome screen with a responsive portrait and session
//! info. Layout mode is selected explicitly by terminal width:
//! - Wide (>= 100 cols): split portrait on the left, metadata on the right
//! - Medium (60-99 cols): compact portrait above metadata
//! - Narrow (< 60 cols): text-only title with no portrait

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};

use crate::history::rgb;

/// Minimum width for the wide side-by-side layout.
const WIDE_LAYOUT_MIN_WIDTH: u16 = 100;

/// Minimum width for the medium (stacked) layout.
const MEDIUM_LAYOUT_MIN_WIDTH: u16 = 60;

const DEFAULT_SPLIT_PORTRAIT_ART: &[&str] = &[
    "       @@@       ",
    "     @@%%%@@     ",
    "    @@%@@@%@@    ",
    "    @%  %  %@    ",
    "    @% /_\\\\ %@    ",
    "   @@%_____%@@   ",
];

const DEFAULT_COMPACT_PORTRAIT_ART: &[&str] = &[
    "    @@@    ",
    "  @@%%%@@  ",
    "  @%/_\\\\%@  ",
    "  @@___@@  ",
];

/// Session info displayed on the welcome screen.
pub struct WelcomeInfo {
    pub model: String,
    pub cwd: String,
    pub version: String,
}

/// Welcome screen widget showing portrait art with session info.
pub struct WelcomeWidget {
    info: WelcomeInfo,
    /// Split portrait lines used in wide mode.
    split_art: Vec<Line<'static>>,
    /// Compact portrait lines used in medium mode.
    compact_art: Vec<Line<'static>>,
    /// Kept for API compatibility; static portraits do not advance frames.
    current_frame: usize,
    /// Static portraits are terminal-native and never animated.
    pub animated: bool,
}

impl WelcomeWidget {
    /// Create a new welcome widget from explicit split and compact portrait art.
    ///
    /// When a portrait slice is empty, a minimal built-in fallback is used for
    /// that mode.
    pub fn new(info: WelcomeInfo, full_art: &[String], compact_art: &[String]) -> Self {
        Self {
            info,
            split_art: parse_portrait_art(full_art, DEFAULT_SPLIT_PORTRAIT_ART),
            compact_art: parse_portrait_art(compact_art, DEFAULT_COMPACT_PORTRAIT_ART),
            current_frame: 0,
            animated: false,
        }
    }

    /// Static portraits do not animate, so advancing frames is a no-op.
    pub fn next_frame(&mut self) {}

    /// Get the current split portrait lines.
    fn current_full_art(&self) -> &[Line<'static>] {
        &self.split_art
    }

    /// Get the current compact portrait lines.
    fn current_compact_art(&self) -> &[Line<'static>] {
        &self.compact_art
    }

    /// Render the welcome screen into the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        if area.width >= WIDE_LAYOUT_MIN_WIDTH {
            self.render_full(area, buf);
        } else if area.width >= MEDIUM_LAYOUT_MIN_WIDTH {
            self.render_compact(area, buf);
        } else {
            self.render_text_only(area, buf);
        }
    }

    /// Full layout: bordered box with art on left, info on right.
    fn render_full(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // Compute art dimensions (visual width of longest line).
        let art = self.current_full_art();
        let art_width = art
            .iter()
            .map(|l| line_visual_width(l))
            .max()
            .unwrap_or(0) as u16;
        let art_height = art.len() as u16;

        // Art on the left, info on the right.
        let art_col_width = art_width.min(inner.width / 2);
        let info_col_x = inner.x + art_col_width + 2; // 2 cols gap
        let info_col_width = inner.width.saturating_sub(art_col_width + 2);

        // Center art vertically within the inner area.
        let art_y = inner.y + inner.height.saturating_sub(art_height) / 2;

        // Render art lines.
        let art_area = Rect {
            x: inner.x,
            y: art_y,
            width: art_col_width,
            height: art_height.min(inner.height),
        };
        let art_paragraph = Paragraph::new(art.to_vec());
        art_paragraph.render(art_area, buf);

        // Build info lines.
        let info_lines = self.info_lines();
        let info_height = info_lines.len() as u16;
        let info_y = inner.y + inner.height.saturating_sub(info_height) / 2;

        let info_area = Rect {
            x: info_col_x,
            y: info_y,
            width: info_col_width,
            height: info_height.min(inner.height),
        };
        let info_paragraph = Paragraph::new(info_lines);
        info_paragraph.render(info_area, buf);
    }

    /// Compact layout: bordered box with art centered above info.
    fn render_compact(&self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let art_lines = self.current_compact_art();
        let art_height = art_lines.len() as u16;
        let info_lines = self.info_lines();
        let info_height = info_lines.len() as u16;
        let gap = 1u16;

        let total_content = art_height + gap + info_height;
        let start_y = inner.y + inner.height.saturating_sub(total_content) / 2;

        // Render compact art centered horizontally.
        let art_visual_width = art_lines
            .iter()
            .map(|l| line_visual_width(l))
            .max()
            .unwrap_or(0) as u16;
        let art_x = inner.x + inner.width.saturating_sub(art_visual_width) / 2;
        let art_area = Rect {
            x: art_x,
            y: start_y,
            width: art_visual_width.min(inner.width),
            height: art_height.min(inner.height),
        };
        let art_paragraph = Paragraph::new(art_lines.to_vec());
        art_paragraph.render(art_area, buf);

        // Render info below art, centered.
        let info_visual_width = info_lines
            .iter()
            .map(|l| line_visual_width(l))
            .max()
            .unwrap_or(0) as u16;
        let info_x = inner.x + inner.width.saturating_sub(info_visual_width) / 2;
        let info_y = start_y + art_height + gap;
        if info_y < inner.y + inner.height {
            let info_area = Rect {
                x: info_x,
                y: info_y,
                width: info_visual_width.min(inner.width),
                height: info_height.min(inner.y + inner.height - info_y),
            };
            let info_paragraph = Paragraph::new(info_lines);
            info_paragraph.render(info_area, buf);
        }
    }

    /// Text-only layout: just ">_ Eve v{version}" in accent color.
    fn render_text_only(&self, area: Rect, buf: &mut Buffer) {
        let accent = rgb(genesis_ui::colors::UI_ACCENT);
        let text = format!(">_ Eve v{}", self.info.version);
        let line = Line::from(Span::styled(text, Style::default().fg(accent)));

        // Center vertically and horizontally.
        let y = area.y + area.height / 2;
        let x = area.x + area.width.saturating_sub(line.width() as u16) / 2;
        if y < area.y + area.height {
            let text_area = Rect {
                x,
                y,
                width: area.width.saturating_sub(x - area.x),
                height: 1,
            };
            let paragraph = Paragraph::new(vec![line]);
            paragraph.render(text_area, buf);
        }
    }

    /// Build the info lines displayed alongside or below the art.
    fn info_lines(&self) -> Vec<Line<'static>> {
        let accent = rgb(genesis_ui::colors::UI_ACCENT);
        let dim = rgb(genesis_ui::colors::UI_DIM);
        let text_color = rgb(genesis_ui::colors::UI_TEXT);

        let title = Line::from(Span::styled(
            format!(">_ Eve v{}", self.info.version),
            Style::default().fg(accent),
        ));
        let blank = Line::from("");
        let model_line = Line::from(vec![
            Span::styled("model: ", Style::default().fg(dim)),
            Span::styled(self.info.model.clone(), Style::default().fg(text_color)),
        ]);
        let cwd_line = Line::from(vec![
            Span::styled("  cwd: ", Style::default().fg(dim)),
            Span::styled(
                truncate_path(&self.info.cwd, 40),
                Style::default().fg(text_color),
            ),
        ]);

        vec![title, blank, model_line, cwd_line]
    }
}

/// Truncate a path string to at most `max_len` characters, replacing the
/// start with "..." if needed.
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        let suffix = &path[path.len() - (max_len - 3)..];
        format!("...{suffix}")
    }
}

/// Compute the visual width of a ratatui Line (sum of span content widths).
fn line_visual_width(line: &Line<'_>) -> usize {
    use unicode_width::UnicodeWidthStr as _;
    line.spans.iter().map(|s| s.content.width()).sum()
}

fn parse_portrait_art(source: &[String], fallback: &[&str]) -> Vec<Line<'static>> {
    if source.is_empty() {
        fallback.iter().map(|line| parse_ansi_line(line)).collect()
    } else {
        parse_ansi_art(source)
    }
}

// ── ANSI → ratatui Line parser ─────────────────────────────────────────────

/// Parse ANSI-escaped half-block art strings into ratatui Lines.
pub fn parse_ansi_art(ansi_lines: &[String]) -> Vec<Line<'static>> {
    ansi_lines.iter().map(|line| parse_ansi_line(line)).collect()
}

/// Parse a single ANSI-escaped line into a ratatui Line with colored Spans.
///
/// Handles:
/// - `\x1b[38;2;R;G;Bm` — set foreground color
/// - `\x1b[48;2;R;G;Bm` — set background color
/// - `\x1b[0m` — reset all styles
/// - Combined: `\x1b[38;2;R;G;Bm\x1b[48;2;R;G;Bm` (fg then bg in sequence)
///
/// Text between escape codes is accumulated into Spans with the current style.
pub fn parse_ansi_line(s: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current_fg: Option<Color> = None;
    let mut current_bg: Option<Color> = None;
    let mut text_buf = String::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == 0x1b && i + 1 < len && bytes[i + 1] == b'[' {
            // Start of an escape sequence.
            // Flush any accumulated text first.
            if !text_buf.is_empty() {
                let style = make_style(current_fg, current_bg);
                spans.push(Span::styled(std::mem::take(&mut text_buf), style));
            }

            // Find the end of the escape sequence (terminated by 'm').
            let seq_start = i + 2; // skip ESC[
            let mut seq_end = seq_start;
            while seq_end < len && bytes[seq_end] != b'm' {
                seq_end += 1;
            }
            if seq_end >= len {
                // Malformed: no terminator. Skip the ESC.
                i += 1;
                continue;
            }

            let seq = &s[seq_start..seq_end];
            if seq == "0" {
                // Reset.
                current_fg = None;
                current_bg = None;
            } else {
                // Parse semicolon-separated numbers.
                let parts: Vec<&str> = seq.split(';').collect();
                if parts.len() == 5 && parts[1] == "2" {
                    // 38;2;R;G;B or 48;2;R;G;B
                    if let (Ok(r), Ok(g), Ok(b)) = (
                        parts[2].parse::<u8>(),
                        parts[3].parse::<u8>(),
                        parts[4].parse::<u8>(),
                    ) {
                        match parts[0] {
                            "38" => current_fg = Some(Color::Rgb(r, g, b)),
                            "48" => current_bg = Some(Color::Rgb(r, g, b)),
                            _ => {}
                        }
                    }
                }
            }

            i = seq_end + 1; // skip past 'm'
        } else {
            // Regular character — accumulate.
            // Handle multi-byte UTF-8 correctly.
            let ch_start = i;
            let c = s[i..].chars().next().unwrap();
            i += c.len_utf8();
            text_buf.push_str(&s[ch_start..i]);
        }
    }

    // Flush remaining text.
    if !text_buf.is_empty() {
        let style = make_style(current_fg, current_bg);
        spans.push(Span::styled(text_buf, style));
    }

    Line::from(spans)
}

/// Build a ratatui Style from optional fg/bg colors.
fn make_style(fg: Option<Color>, bg: Option<Color>) -> Style {
    let mut style = Style::default();
    if let Some(fg) = fg {
        style = style.fg(fg);
    }
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    style
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn parse_ansi_line_plain_text() {
        let line = parse_ansi_line("hello world");
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "hello world");
        assert_eq!(line.spans[0].style, Style::default());
    }

    #[test]
    fn parse_ansi_line_fg_only() {
        let input = "\x1b[38;2;255;0;128mhello\x1b[0m";
        let line = parse_ansi_line(input);
        // Should have: colored span "hello", then nothing after reset
        assert!(!line.spans.is_empty());
        assert_eq!(line.spans[0].content, "hello");
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(Color::Rgb(255, 0, 128))
        );
    }

    #[test]
    fn parse_ansi_line_fg_and_bg() {
        let input = "\x1b[38;2;100;200;50m\x1b[48;2;10;20;30m▀\x1b[0m";
        let line = parse_ansi_line(input);
        assert!(!line.spans.is_empty());
        assert_eq!(line.spans[0].content, "▀");
        assert_eq!(
            line.spans[0].style,
            Style::default()
                .fg(Color::Rgb(100, 200, 50))
                .bg(Color::Rgb(10, 20, 30))
        );
    }

    #[test]
    fn parse_ansi_line_reset_clears_colors() {
        let input = "\x1b[38;2;255;0;0mred\x1b[0mplain";
        let line = parse_ansi_line(input);
        assert_eq!(line.spans.len(), 2);
        assert_eq!(line.spans[0].content, "red");
        assert_eq!(
            line.spans[0].style,
            Style::default().fg(Color::Rgb(255, 0, 0))
        );
        assert_eq!(line.spans[1].content, "plain");
        assert_eq!(line.spans[1].style, Style::default());
    }

    #[test]
    fn parse_ansi_line_mixed_halfblocks_and_spaces() {
        // Simulates: transparent space, then colored ▀, then reset + space
        let input = " \x1b[38;2;180;167;214m▀\x1b[0m ";
        let line = parse_ansi_line(input);
        // Should have: " " plain, "▀" colored, " " plain
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, " ");
        assert_eq!(line.spans[1].content, "▀");
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(Color::Rgb(180, 167, 214))
        );
        assert_eq!(line.spans[2].content, " ");
    }

    #[test]
    fn parse_ansi_art_multiple_lines() {
        let lines = vec![
            "\x1b[38;2;1;2;3m▄\x1b[0m".to_string(),
            "plain".to_string(),
        ];
        let parsed = parse_ansi_art(&lines);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].spans[0].content, "▄");
        assert_eq!(parsed[1].spans[0].content, "plain");
    }

    #[test]
    fn welcome_widget_renders_full_without_panic() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &["\x1b[38;2;180;167;214m▀▀▀\x1b[0m".to_string()],
            &["\x1b[38;2;180;167;214m▀\x1b[0m".to_string()],
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        widget.render(Rect::new(0, 0, 120, 30), &mut buf);
    }

    #[test]
    fn welcome_widget_renders_compact_without_panic() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &["\x1b[38;2;180;167;214m▀▀▀\x1b[0m".to_string()],
            &["\x1b[38;2;180;167;214m▀\x1b[0m".to_string()],
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        widget.render(Rect::new(0, 0, 80, 24), &mut buf);
    }

    #[test]
    fn welcome_widget_renders_text_only_without_panic() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &[],
            &[],
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        widget.render(Rect::new(0, 0, 40, 10), &mut buf);
    }

    #[test]
    fn welcome_widget_wide_render_uses_split_portrait() {
        let split_art = vec!["@@@@".to_string(), "@%%@".to_string()];
        let compact_art = vec!["++++".to_string(), "+==+".to_string()];
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &split_art,
            &compact_art,
        );

        let area = Rect::new(0, 0, 120, 24);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let rendered = buffer_contents(&buf);
        assert!(rendered.contains('@'));
        assert!(!rendered.contains('+'));
    }

    #[test]
    fn welcome_widget_medium_render_uses_compact_portrait() {
        let split_art = vec!["@@@@".to_string(), "@%%@".to_string()];
        let compact_art = vec!["++++".to_string(), "+==+".to_string()];
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &split_art,
            &compact_art,
        );

        let area = Rect::new(0, 0, 80, 24);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let rendered = buffer_contents(&buf);
        assert!(rendered.contains('+'));
        assert!(!rendered.contains('@'));
    }

    #[test]
    fn welcome_widget_narrow_render_omits_portrait_and_keeps_title() {
        let split_art = vec!["@@@@".to_string(), "@%%@".to_string()];
        let compact_art = vec!["++++".to_string(), "+==+".to_string()];
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "gpt-5.4".to_string(),
                cwd: "/home/user/project".to_string(),
                version: "0.1.0".to_string(),
            },
            &split_art,
            &compact_art,
        );

        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let rendered = buffer_contents(&buf);
        assert!(!rendered.contains('@'));
        assert!(!rendered.contains('+'));
        assert!(rendered.contains(">_ Eve v0.1.0"));
    }

    #[test]
    fn welcome_widget_zero_area_does_not_panic() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "m".to_string(),
                cwd: "/".to_string(),
                version: "0".to_string(),
            },
            &[],
            &[],
        );

        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 1));
        widget.render(Rect::new(0, 0, 0, 0), &mut buf);
    }

    #[test]
    fn truncate_path_short_unchanged() {
        assert_eq!(truncate_path("/home/user", 40), "/home/user");
    }

    #[test]
    fn truncate_path_long_gets_ellipsis() {
        let long = "/home/user/very/deeply/nested/project/src/main.rs";
        let result = truncate_path(long, 20);
        assert!(result.starts_with("..."));
        assert_eq!(result.len(), 20);
    }

    #[test]
    fn info_lines_contain_version_and_model() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "claude-4".to_string(),
                cwd: "/tmp".to_string(),
                version: "1.2.3".to_string(),
            },
            &[],
            &[],
        );
        let lines = widget.info_lines();
        // Title line should contain version.
        let title_text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(title_text.contains("1.2.3"));
        // Model line (index 2 since index 1 is blank).
        let model_text: String = lines[2].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(model_text.contains("claude-4"));
    }

    #[test]
    fn next_frame_is_a_no_op_for_static_portraits() {
        let widget_info = WelcomeInfo {
            model: "test".to_string(),
            cwd: "/tmp".to_string(),
            version: "0.0.0".to_string(),
        };
        let mut widget = WelcomeWidget::new(widget_info, &[], &[]);

        assert!(!widget.animated);
        assert_eq!(widget.current_frame, 0);

        widget.next_frame();
        assert_eq!(widget.current_frame, 0);
    }

    fn buffer_contents(buf: &Buffer) -> String {
        buf.content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }
}

//! Welcome screen widget with terminal-native portrait art.
//!
//! Displays a bordered welcome screen with a responsive portrait and session
//! info. Layout mode is selected explicitly by terminal width:
//! - Wide (>= 100 cols): big-text title at top, portrait left, metadata right,
//!   boot status below portrait
//! - Medium (60-99 cols): compact portrait above metadata
//! - Narrow (< 60 cols): text-only title with no portrait

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};
use tui_big_text::{BigText, PixelSize};

use crate::history::rgb;
use crate::widgets::boot_status::{BootStatusInfo, BootStatusWidget};
use crate::widgets::braille_canvas::{BrailleCanvas, Pattern};

/// Minimum width for the wide side-by-side layout.
const WIDE_LAYOUT_MIN_WIDTH: u16 = 100;

/// Minimum width for the medium (stacked) layout.
const MEDIUM_LAYOUT_MIN_WIDTH: u16 = 60;


/// Height of the big-text title in rows (Quadrant pixel size: 8px / 2 = 4 rows).
const BIG_TEXT_HEIGHT: u16 = 4;

/// Gap between the title and the portrait/info row.
const TITLE_GAP: u16 = 1;

/// Session info displayed on the welcome screen.
pub struct WelcomeInfo {
    pub model: String,
    pub backend: String,
    pub session_id: String,
    pub cwd: String,
    pub version: String,
    pub tool_count_builtin: usize,
    pub tool_count_mcp: usize,
    pub skill_count: usize,
}

/// Computed regions from the last render, used to target boot effects.
#[derive(Debug, Default, Clone, Copy)]
pub struct WelcomeAreas {
    /// The big-text title area (or small title area in non-wide modes).
    pub title: Rect,
    /// The portrait art area.
    pub portrait: Rect,
    /// The boot status lines area.
    pub status: Rect,
    /// The full welcome screen area.
    pub full: Rect,
}

/// Welcome screen widget showing session info with big-text title.
pub struct WelcomeWidget {
    info: WelcomeInfo,
    /// Whether the boot sequence has been triggered.
    boot_triggered: bool,
    /// Areas computed during the last render for effect targeting.
    last_areas: WelcomeAreas,
    /// Braille canvas pattern for the welcome screen animation.
    pub braille_pattern: Pattern,
}

impl WelcomeWidget {
    /// Create a new welcome widget from session info.
    pub fn new(info: WelcomeInfo, _full_art: &[String], _compact_art: &[String]) -> Self {
        Self {
            info,
            boot_triggered: false,
            last_areas: WelcomeAreas::default(),
            braille_pattern: Pattern::matrix_rain(50),
        }
    }

    /// Returns `true` if the boot sequence has already been triggered.
    pub fn boot_triggered(&self) -> bool {
        self.boot_triggered
    }

    /// Mark the boot sequence as triggered. Called by the render loop after
    /// starting boot effects.
    pub fn mark_boot_triggered(&mut self) {
        self.boot_triggered = true;
    }

    /// Return the areas computed during the last render call.
    ///
    /// The caller uses these to target boot effects at the correct screen
    /// regions.
    pub fn last_areas(&self) -> WelcomeAreas {
        self.last_areas
    }

    /// Build a [`BootStatusInfo`] from the current [`WelcomeInfo`].
    pub fn boot_status_info(&self) -> BootStatusInfo {
        BootStatusInfo {
            // Provider and storage are assumed OK if we got this far.
            provider_ok: true,
            storage_ok: true,
            tool_count: self.info.tool_count_builtin + self.info.tool_count_mcp,
            mcp_count: self.info.tool_count_mcp,
            skill_count: self.info.skill_count,
        }
    }

    /// Render the welcome screen into the given area.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        self.last_areas.full = area;

        if area.width >= WIDE_LAYOUT_MIN_WIDTH {
            self.render_full(area, buf);
        } else if area.width >= MEDIUM_LAYOUT_MIN_WIDTH {
            self.render_compact(area, buf);
        } else {
            self.render_text_only(area, buf);
        }
    }

    /// Full layout: bordered box with big-text title at top, art on left,
    /// info on right, boot status below portrait.
    fn render_full(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // ── Big-text title at the top ──────────────────────────────
        let title_available_height = BIG_TEXT_HEIGHT.min(inner.height);
        let title_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: title_available_height,
        };
        self.last_areas.title = title_area;

        let big_title = BigText::builder()
            .pixel_size(PixelSize::Quadrant)
            .style(Style::default().fg(rgb(genesis_ui::colors::EVE_LAVENDER)))
            .lines(vec!["GENESIS".into()])
            .centered()
            .build();
        big_title.render(title_area, buf);

        let below_title_y = inner.y + title_available_height + TITLE_GAP;
        let remaining_height = inner
            .height
            .saturating_sub(title_available_height + TITLE_GAP);
        if remaining_height == 0 {
            return;
        }

        self.render_body(inner, below_title_y, remaining_height, buf);
    }

    /// Compact layout: bordered box with big-text title, braille strip, and info.
    fn render_compact(&mut self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        // ── Big-text title at the top ──────────────────────────────
        let title_available_height = BIG_TEXT_HEIGHT.min(inner.height);
        let title_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: title_available_height,
        };
        self.last_areas.title = title_area;

        let big_title = BigText::builder()
            .pixel_size(PixelSize::Quadrant)
            .style(Style::default().fg(rgb(genesis_ui::colors::EVE_LAVENDER)))
            .lines(vec!["GENESIS".into()])
            .centered()
            .build();
        big_title.render(title_area, buf);

        let below_title_y = inner.y + title_available_height + TITLE_GAP;
        let remaining_height = inner
            .height
            .saturating_sub(title_available_height + TITLE_GAP);
        if remaining_height == 0 {
            return;
        }

        self.render_body(inner, below_title_y, remaining_height, buf);
    }

    /// Shared body: matrix rain background + info + boot status, centered.
    fn render_body(
        &mut self,
        inner: Rect,
        below_title_y: u16,
        remaining_height: u16,
        buf: &mut Buffer,
    ) {
        // ── Matrix rain background (full area behind content) ────────
        let rain_area = Rect {
            x: inner.x,
            y: below_title_y,
            width: inner.width,
            height: remaining_height,
        };
        self.last_areas.portrait = rain_area;
        BrailleCanvas::new(&self.braille_pattern).render(rain_area, buf);

        // ── Info + boot status centered on top of rain ───────────────
        let body_y = below_title_y;
        let body_height = remaining_height;
        if body_height == 0 {
            return;
        }

        let boot_status = BootStatusWidget::new(self.boot_status_info());
        let status_lines = boot_status.lines();
        let status_height = status_lines.len() as u16;
        let status_gap = 1u16;

        let info_lines = self.info_lines();
        let info_height = info_lines.len() as u16;

        let total_content = info_height + status_gap + status_height;
        let content_start_y = body_y + body_height.saturating_sub(total_content) / 2;

        // Info panel
        let info_visual_width = info_lines
            .iter()
            .map(|l| line_visual_width(l))
            .max()
            .unwrap_or(0) as u16;
        let info_x = inner.x + inner.width.saturating_sub(info_visual_width) / 2;
        let info_render_height = info_height.min(body_height);
        let info_area = Rect {
            x: info_x,
            y: content_start_y,
            width: info_visual_width.min(inner.width),
            height: info_render_height,
        };
        let info_paragraph = Paragraph::new(info_lines);
        info_paragraph.render(info_area, buf);

        // Boot status below info
        let status_y = content_start_y + info_render_height + status_gap;
        let status_render_height =
            status_height.min((body_y + body_height).saturating_sub(status_y));
        if status_render_height > 0 {
            let status_width = inner.width.min(40);
            let status_x = inner.x + inner.width.saturating_sub(status_width) / 2;
            let status_area = Rect {
                x: status_x,
                y: status_y,
                width: status_width,
                height: status_render_height,
            };
            self.last_areas.status = status_area;
            boot_status.render(status_area, buf);
        }
    }

    /// Text-only layout: just ">_ Eve v{version}" in accent color.
    fn render_text_only(&mut self, area: Rect, buf: &mut Buffer) {
        let accent = rgb(genesis_ui::colors::UI_ACCENT);
        let text = format!(">_ Eve v{}", self.info.version);
        let line = Line::from(Span::styled(text, Style::default().fg(accent)));

        let y = area.y + area.height / 2;
        let x = area.x + area.width.saturating_sub(line.width() as u16) / 2;
        if y < area.y + area.height {
            let text_area = Rect {
                x,
                y,
                width: area.width.saturating_sub(x - area.x),
                height: 1,
            };
            self.last_areas.title = text_area;
            self.last_areas.portrait = text_area;
            self.last_areas.status = text_area;
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
        let session_line = Line::from(vec![
            Span::styled("session: ", Style::default().fg(dim)),
            Span::styled(
                self.info.session_id.clone(),
                Style::default().fg(text_color),
            ),
        ]);
        let backend_line = Line::from(vec![
            Span::styled("backend: ", Style::default().fg(dim)),
            Span::styled(self.info.backend.clone(), Style::default().fg(text_color)),
        ]);
        let model_line = Line::from(vec![
            Span::styled("model: ", Style::default().fg(dim)),
            Span::styled(self.info.model.clone(), Style::default().fg(text_color)),
        ]);
        let cwd_line = Line::from(vec![
            Span::styled("cwd: ", Style::default().fg(dim)),
            Span::styled(
                truncate_path(&self.info.cwd, 40),
                Style::default().fg(text_color),
            ),
        ]);
        let tools_line = Line::from(vec![
            Span::styled("tools: ", Style::default().fg(dim)),
            Span::styled(
                format_tool_summary(self.info.tool_count_builtin, self.info.tool_count_mcp),
                Style::default().fg(text_color),
            ),
        ]);
        let skills_line = Line::from(vec![
            Span::styled("skills: ", Style::default().fg(dim)),
            Span::styled(
                format!("{} installed", self.info.skill_count),
                Style::default().fg(text_color),
            ),
        ]);
        let hint_line_primary = Line::from(vec![
            Span::styled("enter", Style::default().fg(accent)),
            Span::styled(" start chat    ", Style::default().fg(dim)),
            Span::styled("/", Style::default().fg(accent)),
            Span::styled(" commands", Style::default().fg(dim)),
        ]);
        let hint_line_secondary = Line::from(vec![
            Span::styled("esc", Style::default().fg(accent)),
            Span::styled(" dismiss welcome", Style::default().fg(dim)),
        ]);

        vec![
            title,
            blank.clone(),
            session_line,
            backend_line,
            model_line,
            cwd_line,
            tools_line,
            skills_line,
            blank,
            hint_line_primary,
            hint_line_secondary,
        ]
    }
}

fn format_tool_summary(builtin: usize, mcp: usize) -> String {
    let total = builtin + mcp;
    format!("{total} total ({builtin} built-in, {mcp} MCP)")
}

/// Truncate a path string to at most `max_len` characters, splitting at
/// path boundaries so directory names are not chopped mid-word.
///
/// When the full path exceeds `max_len`, the function keeps the last 2-3
/// path segments and prepends `…/` (single-character ellipsis). If even
/// the last segment is too long, it falls back to a simple character cut.
///
/// ## Examples
///
/// ```text
/// /Users/coler/dev-personal/genesis  (max 30)  →  …/dev-personal/genesis
/// /short                              (max 40)  →  /short
/// ```
fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        return path.to_string();
    }

    // The ellipsis prefix occupies 2 characters: "…/"
    const ELLIPSIS_PREFIX: &str = "\u{2026}/";
    const ELLIPSIS_LEN: usize = 2; // "…" is 1 char + "/"

    if max_len <= ELLIPSIS_LEN {
        // Not enough room for even the prefix — fall back to dots.
        return ".".repeat(max_len);
    }

    let budget = max_len - ELLIPSIS_LEN;

    // Split the path into segments and try to fit as many trailing
    // segments as possible within `budget`.
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    let mut included = 0usize; // number of chars for included segments
    let mut count = 0usize; // number of segments included
    for seg in segments.iter().rev() {
        let seg_cost = if count == 0 {
            seg.len()
        } else {
            seg.len() + 1 // +1 for the '/' before it
        };
        if included + seg_cost > budget {
            break;
        }
        included += seg_cost;
        count += 1;
    }

    if count == 0 {
        // Even the last segment alone is too long — fall back to a
        // character-boundary cut of the last segment.
        let last = segments.last().unwrap_or(&path);
        let suffix_chars: String = last
            .chars()
            .rev()
            .take(budget)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return format!("{ELLIPSIS_PREFIX}{suffix_chars}");
    }

    // Build the truncated path from the last `count` segments.
    let start = segments.len() - count;
    let suffix = segments[start..].join("/");
    format!("{ELLIPSIS_PREFIX}{suffix}")
}

/// Compute the visual width of a ratatui Line (sum of span content widths).
fn line_visual_width(line: &Line<'_>) -> usize {
    use unicode_width::UnicodeWidthStr as _;
    line.spans.iter().map(|s| s.content.width()).sum()
}

// ── ANSI → ratatui Line parser ─────────────────────────────────────────────

/// Parse ANSI-escaped half-block art strings into ratatui Lines.
pub fn parse_ansi_art(ansi_lines: &[String]) -> Vec<Line<'static>> {
    ansi_lines
        .iter()
        .map(|line| parse_ansi_line(line))
        .collect()
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
            if !text_buf.is_empty() {
                let style = make_style(current_fg, current_bg);
                spans.push(Span::styled(std::mem::take(&mut text_buf), style));
            }

            let seq_start = i + 2;
            let mut seq_end = seq_start;
            while seq_end < len && bytes[seq_end] != b'm' {
                seq_end += 1;
            }
            if seq_end >= len {
                i += 1;
                continue;
            }

            let seq = &s[seq_start..seq_end];
            if seq == "0" {
                current_fg = None;
                current_bg = None;
            } else {
                let parts: Vec<&str> = seq.split(';').collect();
                if parts.len() == 5 && parts[1] == "2" {
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

            i = seq_end + 1;
        } else {
            let c = s[i..].chars().next().unwrap();
            let ch_start = i;
            i += c.len_utf8();
            text_buf.push_str(&s[ch_start..i]);
        }
    }

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

    fn test_info() -> WelcomeInfo {
        WelcomeInfo {
            session_id: "session-123".to_string(),
            backend: "openai".to_string(),
            model: "gpt-5.4".to_string(),
            cwd: "/home/user/project".to_string(),
            version: "0.1.0".to_string(),
            tool_count_builtin: 61,
            tool_count_mcp: 3,
            skill_count: 12,
        }
    }

    fn sample_widget() -> WelcomeWidget {
        let split_art = vec!["@@@@".to_string(), "@%%@".to_string()];
        let compact_art = vec!["++++".to_string(), "+==+".to_string()];
        WelcomeWidget::new(test_info(), &split_art, &compact_art)
    }

    fn default_widget() -> WelcomeWidget {
        let full = genesis_ui::banner::full_art();
        let compact = genesis_ui::banner::compact_art();
        WelcomeWidget::new(test_info(), &full, &compact)
    }

    fn buffer_rows(buf: &Buffer, width: u16) -> Vec<String> {
        buf.content
            .chunks(width as usize)
            .map(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn first_match_position(rows: &[String], needle: &str) -> Option<(usize, usize)> {
        rows.iter()
            .enumerate()
            .find_map(|(row_idx, row)| row.find(needle).map(|col_idx| (row_idx, col_idx)))
    }

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
        let input = " \x1b[38;2;180;167;214m▀\x1b[0m ";
        let line = parse_ansi_line(input);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, " ");
        assert_eq!(line.spans[1].content, "▀");
        assert_eq!(
            line.spans[1].style,
            Style::default().fg(rgb(genesis_ui::colors::EVE_LAVENDER))
        );
        assert_eq!(line.spans[2].content, " ");
    }

    #[test]
    fn parse_ansi_art_multiple_lines() {
        let lines = vec!["\x1b[38;2;1;2;3m▄\x1b[0m".to_string(), "plain".to_string()];
        let parsed = parse_ansi_art(&lines);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].spans[0].content, "▄");
        assert_eq!(parsed[1].spans[0].content, "plain");
    }

    #[test]
    fn welcome_widget_width_100_uses_wide_layout() {
        let area = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        let rows = buffer_rows(&buf, area.width);
        // Wide layout should render the title and info.
        assert!(
            rows.iter().any(|row| row.contains(">_ Eve v0.1.0")),
            "wide layout should render title"
        );
    }

    #[test]
    fn welcome_widget_width_99_uses_compact_layout() {
        let area = Rect::new(0, 0, 99, 40);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        let rows = buffer_rows(&buf, area.width);
        // With no portrait art, the compact layout should still render the title.
        assert!(
            rows.iter().any(|row| row.contains(">_ Eve v0.1.0")),
            "compact layout should render title"
        );
    }

    #[test]
    fn welcome_widget_width_59_uses_text_only_layout() {
        let area = Rect::new(0, 0, 59, 10);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        let rows = buffer_rows(&buf, area.width);
        // Text-only mode should have no half-block art.
        assert!(
            !rows.iter().any(|row| row.contains('▀') || row.contains('▄')),
            "text-only mode should not render half-block art"
        );
        assert!(rows.iter().any(|row| row.contains(">_ Eve v0.1.0")));
    }

    #[test]
    fn welcome_metadata_renders_all_fields() {
        // Use a taller area to accommodate the new title + portrait + info layout.
        let area = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        let rendered = buffer_rows(&buf, area.width).join("\n");
        for needle in [
            ">_ Eve v0.1.0",
            "session:",
            "session-123",
            "model:",
            "gpt-5.4",
            "backend:",
            "openai",
            "cwd:",
            "/home/user/project",
            "tools:",
            "64 total",
            "skills:",
            "12 installed",
            "enter",
            "commands",
            "esc",
        ] {
            assert!(
                rendered.contains(needle),
                "wide layout should render {needle:?}"
            );
        }
    }

    #[test]
    fn welcome_widget_zero_area_does_not_panic() {
        let mut widget = WelcomeWidget::new(
            WelcomeInfo {
                model: "m".to_string(),
                backend: "b".to_string(),
                session_id: "s".to_string(),
                cwd: "/".to_string(),
                version: "0".to_string(),
                tool_count_builtin: 0,
                tool_count_mcp: 0,
                skill_count: 0,
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
    fn truncate_path_long_uses_path_boundary() {
        let long = "/Users/coler/dev-personal/genesis";
        let result = truncate_path(long, 30);
        // Should keep trailing segments at a path boundary with "…/" prefix.
        assert!(
            result.starts_with('\u{2026}'),
            "should start with ellipsis, got: {result}"
        );
        assert!(
            result.contains("genesis"),
            "should preserve the last segment, got: {result}"
        );
        assert!(
            result.len() <= 30,
            "should fit within budget, got len={}",
            result.len()
        );
    }

    #[test]
    fn truncate_path_preserves_multiple_trailing_segments() {
        let path = "/home/user/very/deeply/nested/project/src/main.rs";
        let result = truncate_path(path, 25);
        // Should include as many trailing segments as fit.
        assert!(result.starts_with('\u{2026}'));
        assert!(result.contains("main.rs"));
        assert!(result.len() <= 25);
    }

    #[test]
    fn truncate_path_very_short_budget() {
        let result = truncate_path("/a/very/long/path", 2);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn info_lines_contain_version_and_model() {
        let widget = WelcomeWidget::new(
            WelcomeInfo {
                session_id: "session-review".to_string(),
                backend: "anthropic".to_string(),
                model: "claude-4".to_string(),
                cwd: "/tmp".to_string(),
                version: "1.2.3".to_string(),
                tool_count_builtin: 61,
                tool_count_mcp: 2,
                skill_count: 7,
            },
            &[],
            &[],
        );
        let lines = widget.info_lines();
        let rendered_lines: Vec<String> = lines.iter().map(line_text).collect();
        assert!(rendered_lines.iter().any(|line| line.contains("1.2.3")));
        assert!(rendered_lines
            .iter()
            .any(|line| line.contains("model: claude-4")));
    }

    #[test]
    fn wide_layout_renders_boot_status_lines() {
        let area = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        let rendered = buffer_rows(&buf, area.width).join("\n");
        assert!(
            rendered.contains("provider"),
            "wide layout should render boot status 'provider' line"
        );
        assert!(
            rendered.contains("storage"),
            "wide layout should render boot status 'storage' line"
        );
    }

    #[test]
    fn wide_layout_renders_big_text_title() {
        // BigText uses block characters. At Quadrant pixel size, "GENESIS"
        // should produce visible characters in the top rows.
        let area = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(area);
        default_widget().render(area, &mut buf);

        // The big text uses block/half-block characters. Check that the title
        // area has non-space content.
        let title_rows: Vec<String> = buffer_rows(&buf, area.width)
            .into_iter()
            .take(5) // Title occupies the top ~5 rows
            .collect();
        let has_content = title_rows.iter().any(|row| {
            row.chars()
                .any(|c| c != ' ' && c != '╭' && c != '╮' && c != '─')
        });
        assert!(
            has_content,
            "big-text title area should have non-space content"
        );
    }

    #[test]
    fn last_areas_populated_after_render() {
        let area = Rect::new(0, 0, 120, 50);
        let mut buf = Buffer::empty(area);
        let mut widget = default_widget();
        widget.render(area, &mut buf);

        let areas = widget.last_areas();
        assert_eq!(areas.full, area, "full area should match viewport");
        assert!(areas.title.width > 0, "title area should have width");
        // Portrait area may be empty when no art is provided.
    }

    #[test]
    fn boot_triggered_starts_false() {
        let widget = default_widget();
        assert!(!widget.boot_triggered());
    }

    #[test]
    fn mark_boot_triggered_sets_flag() {
        let mut widget = default_widget();
        widget.mark_boot_triggered();
        assert!(widget.boot_triggered());
    }
}

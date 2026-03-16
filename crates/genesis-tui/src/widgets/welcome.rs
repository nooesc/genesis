//! Welcome screen widget.
//!
//! Displays a centered, split-screen intro with an ASCII-girl icon and rich
//! startup metadata.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use ratatui::prelude::Widget;

use crate::history::rgb;

// ── Palette ─────────────────────────────────────────────────────────────────

const ACCENT: ratatui::style::Color = rgb(genesis_ui::colors::UI_ACCENT);
const DIM: ratatui::style::Color = rgb(genesis_ui::colors::UI_DIM);
const TEXT: ratatui::style::Color = rgb(genesis_ui::colors::UI_TEXT);
const MUTED: ratatui::style::Color = rgb(genesis_ui::colors::UI_MUTED);
const AMBER: ratatui::style::Color = rgb(genesis_ui::colors::EVE_AMBER);
const SUCCESS: ratatui::style::Color = rgb(genesis_ui::colors::UI_SUCCESS);

const WIDE_LAYOUT_MIN_WIDTH: u16 = 100;
const COMPACT_LAYOUT_MIN_WIDTH: u16 = 60;
const WIDE_LAYOUT_GAP: usize = 4;
const WIDE_INFO_MIN_WIDTH: usize = 40;

const ASCII_GIRL_WIDE: &[&str] = &[
    "           .-''''-.           ",
    "        .-'  .--.  `-.        ",
    "      .'   .'_  _`.   `.      ",
    "     /   .' (o)(o) `.   \\     ",
    "    /   /    /__\\    \\   \\    ",
    "   /   /   .-====-.   \\   \\   ",
    "   |   |  /  .--.  \\  |   |   ",
    "   |   |  |  |  |  |  |   |   ",
    "   |   |  |  |  |  |  |   |   ",
    "   |   |  |  '--'  |  |   |   ",
    "   |   |   \\  __  /   |   |   ",
    "   |   | .-'`----'`-. |   |   ",
    "   |   |/  /| /\\ |\\  \\|   |   ",
    "   |   /  /_|/  \\|_\\  \\   |   ",
    "   |__/__/  /____\\  \\__\\__|   ",
];

const ASCII_GIRL_COMPACT: &[&str] = &[
    "      .-''-.      ",
    "    .' .--. `.    ",
    "   /  /(o )\\  \\   ",
    "  /  /  /_\\ \\  \\  ",
    "  |  | .-==-.| |  ",
    "  |  | | -- || |  ",
    "  |  | |____|| |  ",
    "  |  | /_||_\\\\ |  ",
    "  `--'/_/  \\_\\\\'  ",
];

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

/// Welcome screen widget showing an ASCII signature and session info.
pub struct WelcomeWidget {
    info: WelcomeInfo,
}

impl WelcomeWidget {
    /// Create a new welcome widget from session info.
    pub fn new(info: WelcomeInfo) -> Self {
        Self { info }
    }

    /// Render the welcome screen into the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = if area.width >= WIDE_LAYOUT_MIN_WIDTH {
            self.build_split_lines(area.width)
        } else if area.width >= COMPACT_LAYOUT_MIN_WIDTH {
            self.build_centered_lines(area.width)
        } else {
            self.build_text_only_lines()
        };

        if lines.is_empty() {
            return;
        }

        let content_height = lines.len() as u16;
        let render_height = content_height.min(area.height);
        let start_y = area.y + area.height.saturating_sub(render_height) / 2;
        let text_area = Rect {
            x: area.x,
            y: start_y,
            width: area.width,
            height: render_height,
        };

        let align = if area.width >= WIDE_LAYOUT_MIN_WIDTH {
            Alignment::Left
        } else {
            Alignment::Center
        };
        let paragraph = Paragraph::new(lines).alignment(align);
        paragraph.render(text_area, buf);
    }

    fn build_split_lines(&self, width: u16) -> Vec<Line<'static>> {
        let art_width = ascii_width_max(ASCII_GIRL_WIDE);
        let info = self.build_split_info_lines();
        let mut rows = Vec::new();

        let max_rows = info.len().max(ASCII_GIRL_WIDE.len());
        let right_width = width as usize;
        let reserved_info = WIDE_INFO_MIN_WIDTH.min(right_width);
        let available_left = right_width
            .saturating_sub(reserved_info)
            .saturating_sub(WIDE_LAYOUT_GAP);
        let left_width = art_width.min(available_left.max(art_width.min(right_width)));
        let info_width = right_width
            .saturating_sub(left_width)
            .saturating_sub(WIDE_LAYOUT_GAP)
            .max(1);

        for i in 0..max_rows {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let art_line = ASCII_GIRL_WIDE.get(i).copied().unwrap_or("");
            let left = pad_right(art_line, left_width);
            spans.push(Span::styled(
                left,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" ".repeat(WIDE_LAYOUT_GAP)));

            if let Some(info_line) = info.get(i) {
                let mut clipped = info_line.clone();
                let mut remaining = info_width;
                let mut spans_left = Vec::new();

                for mut span in clipped.spans.drain(..) {
                    if remaining == 0 {
                        break;
                    }
                    let clipped_text = clip_text(span.content.as_ref(), remaining);
                    let spent = visible_width(&clipped_text);
                    remaining = remaining.saturating_sub(spent);
                    span.content = clipped_text.into();
                    spans_left.push(span);
                }

                clipped.spans = spans_left;
                spans.extend(clipped.spans.into_iter());
            }
            rows.push(Line::from(spans));
        }

        rows
    }

    fn build_centered_lines(&self, _width: u16) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        lines.push(Line::from(Span::styled(
            format!(">_ Eve v{}", self.info.version),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        for art in ASCII_GIRL_COMPACT {
            lines.push(Line::from(Span::styled(
                art.to_owned(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));

        lines.extend(self.build_info_lines());
        lines.push(Line::from(""));
        lines.extend(self.build_hint_lines());

        lines
    }

    fn build_text_only_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(">_ Eve v{}", self.info.version),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("model       ", Style::default().fg(DIM)),
            Span::styled(self.info.model.clone(), Style::default().fg(TEXT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("backend     ", Style::default().fg(DIM)),
            Span::styled(self.info.backend.clone(), Style::default().fg(TEXT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("cwd         ", Style::default().fg(DIM)),
            Span::styled(
                truncate_path(&self.info.cwd, 40),
                Style::default().fg(TEXT),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "press any key to start",
            Style::default().fg(DIM),
        )));
        lines
    }

    fn build_split_info_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(">_ Eve v{}", self.info.version),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.extend(self.build_info_lines());
        lines.push(Line::from(""));
        lines.extend(self.build_hint_lines());
        lines
    }

    fn build_info_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "session"), Style::default().fg(DIM)),
            Span::styled(self.info.session_id.clone(), Style::default().fg(TEXT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "model"), Style::default().fg(DIM)),
            Span::styled(self.info.model.clone(), Style::default().fg(TEXT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "backend"), Style::default().fg(DIM)),
            Span::styled(self.info.backend.clone(), Style::default().fg(TEXT)),
        ]));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "cwd"), Style::default().fg(DIM)),
            Span::styled(
                truncate_path(&self.info.cwd, 64),
                Style::default().fg(TEXT),
            ),
        ]));

        let tools = if self.info.tool_count_mcp > 0 {
            format!(
                "{} builtin, {} mcp",
                self.info.tool_count_builtin,
                self.info.tool_count_mcp
            )
        } else {
            format!("{} builtin", self.info.tool_count_builtin)
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "tools"), Style::default().fg(DIM)),
            Span::styled(tools, Style::default().fg(SUCCESS)),
        ]));

        lines.push(Line::from(vec![
            Span::styled(format!("{:<12}", "skills"), Style::default().fg(DIM)),
            Span::styled(
                self.info.skill_count.to_string(),
                Style::default().fg(MUTED),
            ),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "session ready",
            Style::default()
                .fg(AMBER)
                .add_modifier(Modifier::ITALIC),
        )));
        lines
    }

    fn build_hint_lines(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        let key_style = Style::default().fg(ACCENT);
        let desc_style = Style::default().fg(MUTED);
        let hints = [
            ("Enter", "send message"),
            ("/", "slash commands"),
            ("Ctrl+T", "transcript"),
            ("Ctrl+C", "interrupt"),
            ("Ctrl+D", "exit"),
        ];

        for (key, desc) in &hints {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>10}", key), key_style),
                Span::styled(format!("  {desc}"), desc_style),
            ]));
        }
        lines.push(Line::from(Span::styled(
            "press any key to start",
            Style::default().fg(DIM),
        )));
        lines
    }
}

fn pad_right(value: &str, width: usize) -> String {
    let visible_width = value.chars().count();
    if width <= visible_width {
        value.to_owned()
    } else {
        let mut out = value.to_owned();
        out.push_str(&" ".repeat(width - visible_width));
        out
    }
}

fn visible_width(value: &str) -> usize {
    value.chars().count()
}

fn ascii_width_max(lines: &[&str]) -> usize {
    lines.iter().map(|line| visible_width(line)).max().unwrap_or(0)
}

fn clip_text(value: &str, max_chars: usize) -> String {
    if visible_width(value) <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

/// Truncate a path string to at most `max_len` characters.
fn truncate_path(path: &str, max_len: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max_len {
        path.to_owned()
    } else {
        let start = chars.len().saturating_sub(max_len - 3);
        let suffix: String = chars[start..].iter().collect();
        format!("...{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_rows(buf: &Buffer, width: u16) -> Vec<String> {
        buf.content
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<Vec<_>>().join(""))
            .collect()
    }

    fn rendered_text(buf: &Buffer, width: u16) -> String {
        buffer_rows(buf, width).join("\n")
    }

    fn sample_info() -> WelcomeInfo {
        WelcomeInfo {
            model: "gpt-5.4".to_string(),
            backend: "openrouter".to_string(),
            session_id: "cli-20260315-abcdef".to_string(),
            cwd: "/home/user/project".to_string(),
            version: "0.1.0".to_string(),
            tool_count_builtin: 58,
            tool_count_mcp: 4,
            skill_count: 12,
        }
    }

    #[test]
    fn welcome_widget_renders_without_panic() {
        let widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        widget.render(Rect::new(0, 0, 120, 30), &mut buf);
        let rendered = rendered_text(&buf, 120);
        assert!(rendered.contains(">_ Eve v0.1.0"));
        assert!(rendered.contains(".-'  .--.  `-."));
        assert!(rendered.contains("Enter"));
    }

    #[test]
    fn welcome_widget_renders_narrow_without_panic() {
        let widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        widget.render(Rect::new(0, 0, 40, 10), &mut buf);
        let rendered = rendered_text(&buf, 40);
        assert!(rendered.contains(">_ Eve v0.1.0"));
        assert!(!rendered.contains(".-'  .--.  `-."));
        assert!(!rendered.contains(".-''-."));
    }

    #[test]
    fn welcome_widget_compact_mode_uses_compact_portrait() {
        let widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        widget.render(Rect::new(0, 0, 80, 24), &mut buf);
        let rendered = rendered_text(&buf, 80);
        assert!(rendered.contains(".-''-."));
        assert!(rendered.contains("/  /(o )\\  \\"));
        assert!(!rendered.contains(".-'  .--.  `-."));
    }

    #[test]
    fn welcome_widget_zero_area_does_not_panic() {
        let widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        widget.render(Rect::new(0, 0, 0, 0), &mut buf);
    }
}

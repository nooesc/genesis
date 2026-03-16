//! Welcome screen widget.
//!
//! Displays a centered startup screen with image-derived Eve art and rich
//! session metadata.

use std::time::{Duration, Instant};

use ratatui::prelude::Widget;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::history::rgb;

const ACCENT: Color = rgb(genesis_ui::colors::UI_ACCENT);
const DIM: Color = rgb(genesis_ui::colors::UI_DIM);
const TEXT: Color = rgb(genesis_ui::colors::UI_TEXT);
const MUTED: Color = rgb(genesis_ui::colors::UI_MUTED);
const AMBER: Color = rgb(genesis_ui::colors::EVE_AMBER);
const SUCCESS: Color = rgb(genesis_ui::colors::UI_SUCCESS);

const WIDE_LAYOUT_MIN_WIDTH: u16 = 100;
const COMPACT_LAYOUT_MIN_WIDTH: u16 = 70;
const WIDE_LAYOUT_GAP: usize = 4;
const WIDE_INFO_MIN_WIDTH: usize = 40;
const WELCOME_ANIMATION_INTERVAL: Duration = Duration::from_millis(350);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WelcomeArtVariant {
    Wide,
    Compact,
}

struct WelcomeArtCache {
    variant: WelcomeArtVariant,
    width: u16,
    height: u16,
    frames: Vec<Vec<Line<'static>>>,
}

/// Welcome screen widget showing animated terminal art and session info.
pub struct WelcomeWidget {
    info: WelcomeInfo,
    frame_index: usize,
    last_frame_advance: Instant,
    art_cache: Option<WelcomeArtCache>,
}

impl WelcomeWidget {
    /// Create a new welcome widget from session info.
    pub fn new(info: WelcomeInfo) -> Self {
        Self {
            info,
            frame_index: 0,
            last_frame_advance: Instant::now(),
            art_cache: None,
        }
    }

    pub fn animation_interval(&self) -> Duration {
        WELCOME_ANIMATION_INTERVAL
    }

    pub fn tick(&mut self) {
        if self.last_frame_advance.elapsed() >= WELCOME_ANIMATION_INTERVAL {
            self.frame_index = (self.frame_index + 1) % genesis_ui::banner::WELCOME_FRAME_COUNT;
            self.last_frame_advance = Instant::now();
        }
    }

    /// Render the welcome screen into the given area.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let lines = if area.width >= WIDE_LAYOUT_MIN_WIDTH {
            self.build_split_lines(area)
        } else if area.width >= COMPACT_LAYOUT_MIN_WIDTH {
            self.build_centered_lines(area)
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
        Paragraph::new(lines).alignment(align).render(text_area, buf);
    }

    fn build_split_lines(&mut self, area: Rect) -> Vec<Line<'static>> {
        let right_width = area.width as usize;
        let reserved_info = WIDE_INFO_MIN_WIDTH.min(right_width);
        let available_left = right_width
            .saturating_sub(reserved_info)
            .saturating_sub(WIDE_LAYOUT_GAP)
            .max(16);
        let max_art_size = area.height.saturating_sub(6).clamp(12, 24);
        let art_size = available_left.min(max_art_size as usize) as u16;
        let art_lines = self
            .current_art_lines(WelcomeArtVariant::Wide, art_size, art_size)
            .map(|lines| lines.to_vec())
            .unwrap_or_default();

        if art_lines.is_empty() {
            return self.build_text_only_lines();
        }

        let info = self.build_split_info_lines();
        let left_width = art_size as usize;
        let info_width = right_width
            .saturating_sub(left_width)
            .saturating_sub(WIDE_LAYOUT_GAP)
            .max(1);

        let mut rows = Vec::new();
        let max_rows = info.len().max(art_lines.len());

        for index in 0..max_rows {
            let mut spans = if let Some(art_line) = art_lines.get(index) {
                art_line.spans.clone()
            } else {
                vec![Span::raw(" ".repeat(left_width))]
            };
            spans.push(Span::raw(" ".repeat(WIDE_LAYOUT_GAP)));

            if let Some(info_line) = info.get(index) {
                spans.extend(clipped_spans(info_line, info_width));
            }

            rows.push(Line::from(spans));
        }

        rows
    }

    fn build_centered_lines(&mut self, area: Rect) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(">_ Eve v{}", self.info.version),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        let info_lines = self.build_info_lines();
        let hint_lines = self.build_hint_lines();
        let reserved_text_rows =
            (info_lines.len() + hint_lines.len() + 4).min(area.height as usize) as u16;
        let art_size = area
            .width
            .saturating_sub(10)
            .min(area.height.saturating_sub(reserved_text_rows))
            .clamp(10, 18);

        if let Some(art_lines) = self.current_art_lines(WelcomeArtVariant::Compact, art_size, art_size)
        {
            lines.extend(art_lines.iter().cloned());
            lines.push(Line::from(""));
        }

        lines.extend(info_lines);
        lines.push(Line::from(""));
        lines.extend(hint_lines);
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
        let key_style = Style::default().fg(ACCENT);
        let desc_style = Style::default().fg(MUTED);
        let hints = [
            ("Enter", "send message"),
            ("/", "slash commands"),
            ("Ctrl+T", "transcript"),
            ("Ctrl+C", "interrupt"),
            ("Ctrl+D", "exit"),
        ];

        let mut lines = Vec::new();
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

    fn current_art_lines(
        &mut self,
        variant: WelcomeArtVariant,
        width: u16,
        height: u16,
    ) -> Option<&[Line<'static>]> {
        if width == 0 || height == 0 {
            return None;
        }

        let needs_refresh = self.art_cache.as_ref().map_or(true, |cache| {
            cache.variant != variant || cache.width != width || cache.height != height
        });

        if needs_refresh {
            let frames = genesis_ui::banner::render_welcome_frames(width, height)
                .into_iter()
                .map(|frame| halfblock_frame_to_lines(&frame))
                .collect::<Vec<_>>();
            if frames.is_empty() {
                self.art_cache = None;
                return None;
            }
            self.art_cache = Some(WelcomeArtCache {
                variant,
                width,
                height,
                frames,
            });
        }

        self.art_cache
            .as_ref()
            .and_then(|cache| cache.frames.get(self.frame_index % cache.frames.len()))
            .map(Vec::as_slice)
    }
}

fn halfblock_frame_to_lines(frame: &genesis_ui::banner::HalfBlockFrame) -> Vec<Line<'static>> {
    frame
        .lines
        .iter()
        .map(|row| {
            let spans = row
                .iter()
                .map(|cell| {
                    let mut style = Style::default();
                    if let Some(fg) = cell.fg {
                        style = style.fg(Color::Rgb(fg.r, fg.g, fg.b));
                    }
                    if let Some(bg) = cell.bg {
                        style = style.bg(Color::Rgb(bg.r, bg.g, bg.b));
                    }
                    Span::styled(cell.symbol.to_string(), style)
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

fn clipped_spans(line: &Line<'static>, max_chars: usize) -> Vec<Span<'static>> {
    let mut remaining = max_chars;
    let mut out = Vec::new();

    for mut span in line.spans.clone() {
        if remaining == 0 {
            break;
        }
        let clipped = clip_text(span.content.as_ref(), remaining);
        let spent = visible_width(&clipped);
        remaining = remaining.saturating_sub(spent);
        span.content = clipped.into();
        out.push(span);
    }

    out
}

fn visible_width(value: &str) -> usize {
    value.chars().count()
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
    fn welcome_widget_renders_image_backed_wide_layout() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 120, 30));
        widget.render(Rect::new(0, 0, 120, 30), &mut buf);
        let rendered = rendered_text(&buf, 120);
        assert!(rendered.contains(">_ Eve v0.1.0"));
        assert!(rendered.contains("session"));
        assert!(rendered.contains("Enter"));
        assert!(rendered.contains("▀") || rendered.contains("▄"));
    }

    #[test]
    fn welcome_widget_renders_text_only_when_narrow() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 10));
        widget.render(Rect::new(0, 0, 40, 10), &mut buf);
        let rendered = rendered_text(&buf, 40);
        assert!(rendered.contains(">_ Eve v0.1.0"));
        assert!(!rendered.contains("▀"));
        assert!(!rendered.contains("▄"));
    }

    #[test]
    fn welcome_widget_compact_mode_uses_rendered_frame() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        widget.render(Rect::new(0, 0, 80, 24), &mut buf);
        let rendered = rendered_text(&buf, 80);
        assert!(rendered.contains("press any key to start"));
        assert!(rendered.contains("▀") || rendered.contains("▄"));
    }

    #[test]
    fn welcome_widget_tick_advances_frame_index() {
        let mut widget = WelcomeWidget::new(sample_info());
        widget.last_frame_advance = Instant::now() - WELCOME_ANIMATION_INTERVAL;
        widget.tick();
        assert_eq!(widget.frame_index, 1);
    }

    #[test]
    fn welcome_widget_zero_area_does_not_panic() {
        let mut widget = WelcomeWidget::new(sample_info());
        let mut buf = Buffer::empty(Rect::new(0, 0, 0, 0));
        widget.render(Rect::new(0, 0, 0, 0), &mut buf);
    }
}

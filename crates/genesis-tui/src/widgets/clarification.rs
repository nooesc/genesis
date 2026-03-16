//! Interactive clarification picker widget.
//!
//! Shown when the agent asks a clarifying question with choices. The user
//! can arrow-key through options, press a number key to jump, or type a
//! custom answer. Enter confirms the selection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};
use unicode_width::UnicodeWidthChar;

// ── Colors ──────────────────────────────────────────────────────────────────

const BORDER_FG: Color = Color::Rgb(108, 108, 108);
const QUESTION_FG: Color = Color::Rgb(208, 208, 208);
const OPTION_FG: Color = Color::Rgb(180, 167, 214);
const OPTION_DIM: Color = Color::Rgb(138, 138, 138);
const SELECTED_BG: Color = Color::Rgb(50, 45, 65);
const SELECTED_FG: Color = Color::Rgb(230, 225, 240);
const HINT_FG: Color = Color::Rgb(98, 98, 98);
const INPUT_FG: Color = Color::Rgb(208, 208, 208);
const CURSOR_STYLE: Style = Style::new()
    .fg(Color::Rgb(208, 208, 208))
    .add_modifier(Modifier::REVERSED);

// ── Action ──────────────────────────────────────────────────────────────────

/// Result from handling a key event.
#[derive(Debug, PartialEq, Eq)]
pub enum ClarificationAction {
    /// Key was consumed, no further action.
    None,
    /// User confirmed a choice — the string is their answer.
    Confirm(String),
    /// User dismissed/cancelled (Esc).
    Dismiss,
}

// ── Widget ──────────────────────────────────────────────────────────────────

/// Interactive clarification picker.
pub struct ClarificationWidget {
    /// The question text (displayed above options).
    question: String,
    /// Parsed option labels (if any).
    options: Vec<String>,
    /// Currently highlighted option index (`options.len()` = custom input row).
    selected: usize,
    /// Custom text input buffer (when user wants to type a free-form answer).
    custom_input: String,
    /// Whether the widget is visible.
    visible: bool,
}

impl ClarificationWidget {
    /// Create a hidden widget.
    pub fn new() -> Self {
        Self {
            question: String::new(),
            options: Vec::new(),
            selected: 0,
            custom_input: String::new(),
            visible: false,
        }
    }

    /// Show the picker with a clarification question.
    ///
    /// Parses numbered options from the question text (lines matching `  N. ...`).
    pub fn show(&mut self, raw_question: &str) {
        let mut question_lines = Vec::new();
        let mut options = Vec::new();

        for line in raw_question.lines() {
            let trimmed = line.trim();
            // Match "N. option text" pattern.
            if let Some(rest) = try_parse_option(trimmed) {
                options.push(rest);
            } else if trimmed == "Options:" || trimmed == "[Clarification needed]" {
                // Skip these structural lines.
            } else if !trimmed.is_empty() {
                question_lines.push(trimmed.to_string());
            }
        }

        self.question = question_lines.join("\n");
        self.options = options;
        self.selected = 0;
        self.custom_input.clear();
        self.visible = true;
    }

    /// Hide the picker.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Whether the picker is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> ClarificationAction {
        let total_rows = self.options.len() + 1; // options + custom input row

        match (key.code, key.modifiers) {
            // Navigate up.
            (KeyCode::Up, _) => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                ClarificationAction::None
            }
            // Navigate down.
            (KeyCode::Down, _) => {
                if self.selected + 1 < total_rows {
                    self.selected += 1;
                }
                ClarificationAction::None
            }
            // Number keys jump to option.
            (KeyCode::Char(c), KeyModifiers::NONE) if c.is_ascii_digit() => {
                let n = c as usize - '0' as usize;
                if n >= 1 && n <= self.options.len() {
                    self.selected = n - 1;
                }
                // If on custom input row, also type the digit.
                if self.selected == self.options.len() {
                    self.custom_input.push(c);
                }
                ClarificationAction::None
            }
            // Enter confirms.
            (KeyCode::Enter, _) => {
                let answer = if self.selected < self.options.len() {
                    self.options[self.selected].clone()
                } else {
                    // Custom input row.
                    let text = self.custom_input.trim().to_string();
                    if text.is_empty() {
                        return ClarificationAction::None;
                    }
                    text
                };
                self.hide();
                ClarificationAction::Confirm(answer)
            }
            // Escape dismisses.
            (KeyCode::Esc, _) => {
                self.hide();
                ClarificationAction::Dismiss
            }
            // Backspace on custom input.
            (KeyCode::Backspace, _) => {
                if self.selected == self.options.len() {
                    self.custom_input.pop();
                }
                ClarificationAction::None
            }
            // Character input (on custom row).
            (KeyCode::Char(c), mods)
                if (mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT)
                    && self.selected == self.options.len() =>
            {
                self.custom_input.push(c);
                ClarificationAction::None
            }
            // Tab to jump to custom input row.
            (KeyCode::Tab, _) => {
                self.selected = self.options.len();
                ClarificationAction::None
            }
            _ => ClarificationAction::None,
        }
    }

    /// Render the picker centered in the given area.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.visible || area.width < 10 || area.height < 5 {
            return;
        }

        // Compute popup dimensions.
        let popup_width = (area.width - 4).min(60);
        let content_rows = 2 // question (at least 1 line) + blank
            + self.options.len() as u16
            + 2 // custom input row + hint
            + 2; // top/bottom border
        let popup_height = content_rows.min(area.height - 2);

        // Anchor near the bottom of the area (above input/status),
        // so the chat history above remains readable on tall terminals.
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let bottom_margin = 5u16; // leave space for input box + status bar
        let popup_y = area
            .y
            .saturating_add(area.height)
            .saturating_sub(popup_height + bottom_margin)
            .max(area.y);

        let popup = Rect {
            x: popup_x,
            y: popup_y,
            width: popup_width,
            height: popup_height,
        };

        // Clear popup area with background.
        let bg_style = Style::default().bg(Color::Rgb(28, 26, 34));
        for y in popup.y..popup.y + popup.height {
            for x in popup.x..popup.x + popup.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(' ');
                    cell.set_style(bg_style);
                }
            }
        }

        // Draw border.
        let border_style = Style::default().fg(BORDER_FG).bg(Color::Rgb(28, 26, 34));
        draw_border(popup, border_style, " Eve needs input ", buf);

        // Inner area.
        let inner = Rect {
            x: popup.x + 2,
            y: popup.y + 1,
            width: popup.width.saturating_sub(4),
            height: popup.height.saturating_sub(2),
        };
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let mut row = inner.y;

        // Question text.
        let q_style = Style::default().fg(QUESTION_FG).bg(Color::Rgb(28, 26, 34));
        let q_lines: Vec<Line<'static>> = self
            .question
            .lines()
            .map(|l| Line::from(Span::styled(l.to_owned(), q_style)))
            .collect();
        let q_height = (q_lines.len() as u16).min(inner.height);
        if q_height > 0 {
            let q_area = Rect { x: inner.x, y: row, width: inner.width, height: q_height };
            Paragraph::new(q_lines).wrap(Wrap { trim: false }).render(q_area, buf);
            row += q_height;
        }

        // Blank line.
        if row < inner.y + inner.height {
            row += 1;
        }

        // Options.
        let opt_bg = Color::Rgb(28, 26, 34);
        for (i, option) in self.options.iter().enumerate() {
            if row >= inner.y + inner.height {
                break;
            }

            let is_selected = i == self.selected;
            let (fg, bg, indicator) = if is_selected {
                (SELECTED_FG, SELECTED_BG, "▸ ")
            } else {
                (OPTION_FG, opt_bg, "  ")
            };

            let label = format!("{indicator}{}. {option}", i + 1);
            let style = Style::default().fg(fg).bg(bg);

            // Fill row with background.
            for x in inner.x..inner.x + inner.width {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }

            // Write label.
            let mut x = inner.x;
            for ch in label.chars() {
                if x >= inner.x + inner.width {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                x += UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            }

            row += 1;
        }

        // Blank line before custom input.
        if row < inner.y + inner.height {
            row += 1;
        }

        // Custom input row.
        if row < inner.y + inner.height {
            let is_custom_selected = self.selected == self.options.len();
            let prefix = if is_custom_selected { "▸ " } else { "  " };
            let label = format!("{prefix}Or type: ");
            let (fg, bg) = if is_custom_selected {
                (SELECTED_FG, SELECTED_BG)
            } else {
                (OPTION_DIM, opt_bg)
            };
            let style = Style::default().fg(fg).bg(bg);
            let input_style = Style::default().fg(INPUT_FG).bg(bg);

            // Fill row.
            for x in inner.x..inner.x + inner.width {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(' ');
                    cell.set_style(style);
                }
            }

            // Write prefix.
            let mut x = inner.x;
            for ch in label.chars() {
                if x >= inner.x + inner.width { break; }
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(ch);
                    cell.set_style(style);
                }
                x += UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            }

            // Write custom input text.
            for ch in self.custom_input.chars() {
                if x >= inner.x + inner.width { break; }
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(ch);
                    cell.set_style(input_style);
                }
                x += UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            }

            // Cursor.
            if is_custom_selected && x < inner.x + inner.width {
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(' ');
                    cell.set_style(CURSOR_STYLE);
                }
            }

            row += 1;
        }

        // Hint line.
        if row < inner.y + inner.height {
            let hint = "↑↓ navigate · 1-9 jump · Enter confirm · Esc cancel · Tab custom";
            let hint_style = Style::default().fg(HINT_FG).bg(opt_bg);
            let mut x = inner.x;
            for ch in hint.chars() {
                if x >= inner.x + inner.width { break; }
                if let Some(cell) = buf.cell_mut((x, row)) {
                    cell.set_char(ch);
                    cell.set_style(hint_style);
                }
                x += UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
            }
        }
    }
}

impl Default for ClarificationWidget {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Try to parse a line as a numbered option: `N. text` → Some(text).
fn try_parse_option(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let dot_pos = trimmed.find(". ")?;
    let digits = &trimmed[..dot_pos];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(trimmed[dot_pos + 2..].to_string())
}

/// Draw a rounded border with an optional title.
fn draw_border(area: Rect, style: Style, title: &str, buf: &mut Buffer) {
    let right = area.x + area.width - 1;
    let bottom = area.y + area.height - 1;

    // Corners.
    set_cell(buf, area.x, area.y, "╭", style);
    set_cell(buf, right, area.y, "╮", style);
    set_cell(buf, area.x, bottom, "╰", style);
    set_cell(buf, right, bottom, "╯", style);

    // Top edge with title.
    let title_start = area.x + 2;
    let title_style = Style::default()
        .fg(Color::Rgb(180, 167, 214))
        .bg(style.bg.unwrap_or(Color::Reset));
    let mut x = area.x + 1;
    let mut title_idx = 0;
    let title_chars: Vec<char> = title.chars().collect();
    while x < right {
        if x >= title_start && title_idx < title_chars.len() {
            set_cell(buf, x, area.y, &title_chars[title_idx].to_string(), title_style);
            title_idx += 1;
        } else {
            set_cell(buf, x, area.y, "─", style);
        }
        x += 1;
    }

    // Bottom edge.
    for x in (area.x + 1)..right {
        set_cell(buf, x, bottom, "─", style);
    }

    // Side edges.
    for y in (area.y + 1)..bottom {
        set_cell(buf, area.x, y, "│", style);
        set_cell(buf, right, y, "│", style);
    }
}

fn set_cell(buf: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_symbol(symbol);
        cell.set_style(style);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_option_valid() {
        assert_eq!(try_parse_option("1. Charlotte, NC"), Some("Charlotte, NC".to_string()));
        assert_eq!(try_parse_option("  2. Option B"), Some("Option B".to_string()));
    }

    #[test]
    fn parse_option_invalid() {
        assert_eq!(try_parse_option("no number"), None);
        assert_eq!(try_parse_option("Options:"), None);
    }

    #[test]
    fn show_parses_question_and_options() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nWhich one?\n\nOptions:\n  1. A\n  2. B\n  3. C");
        assert!(w.is_visible());
        assert_eq!(w.question, "Which one?");
        assert_eq!(w.options, vec!["A", "B", "C"]);
        assert_eq!(w.selected, 0);
    }

    #[test]
    fn arrow_keys_navigate() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nPick:\n\nOptions:\n  1. X\n  2. Y");
        assert_eq!(w.selected, 0);
        w.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(w.selected, 1);
        w.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(w.selected, 2); // custom input row
        w.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(w.selected, 1);
    }

    #[test]
    fn number_key_jumps() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nPick:\n\nOptions:\n  1. A\n  2. B\n  3. C");
        w.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
        assert_eq!(w.selected, 1); // index 1 = option "B"
    }

    #[test]
    fn enter_confirms_option() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nPick:\n\nOptions:\n  1. A\n  2. B");
        w.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let action = w.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ClarificationAction::Confirm("B".to_string()));
        assert!(!w.is_visible());
    }

    #[test]
    fn enter_confirms_custom_input() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nPick:\n\nOptions:\n  1. A");
        // Go to custom input row.
        w.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(w.selected, 1); // custom row
        w.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        w.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        let action = w.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ClarificationAction::Confirm("hi".to_string()));
    }

    #[test]
    fn esc_dismisses() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nQ?\n\nOptions:\n  1. A");
        let action = w.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, ClarificationAction::Dismiss);
        assert!(!w.is_visible());
    }

    #[test]
    fn no_options_still_works() {
        let mut w = ClarificationWidget::new();
        w.show("[Clarification needed]\nWhat do you want?");
        assert!(w.options.is_empty());
        assert_eq!(w.selected, 0); // custom input row (only row)
        w.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let action = w.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ClarificationAction::Confirm("x".to_string()));
    }
}

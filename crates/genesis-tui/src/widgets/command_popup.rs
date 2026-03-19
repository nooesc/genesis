//! Slash command popup widget.
//!
//! Appears as a floating box above the input row when the user types `/` as
//! the first character of the input buffer. Offers a filtered list of built-in
//! slash commands with up/down navigation and Enter to select.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::history::rgb;

// ── Colour constants ──────────────────────────────────────────────────────

/// Background of the selected row.
const SELECTED_BG: ratatui::style::Color = rgb(genesis_ui::colors::UI_ACCENT);
/// Foreground for command names (white-ish).
const NAME_FG: ratatui::style::Color = rgb(genesis_ui::colors::UI_TEXT);
/// Foreground for command descriptions (dim).
const DESC_FG: ratatui::style::Color = rgb(genesis_ui::colors::UI_DIM);
/// Foreground for the selection indicator `>`.
const INDICATOR_FG: ratatui::style::Color = rgb(genesis_ui::colors::UI_ACCENT);
/// Border colour.
const BORDER_FG: ratatui::style::Color = rgb(genesis_ui::colors::UI_MUTED);
/// Text colour used on the selected row (dark background → light text).
const SELECTED_TEXT_FG: ratatui::style::Color = rgb(genesis_ui::colors::EVE_DARK);

// ── Built-in command list ─────────────────────────────────────────────────

/// Metadata for a single slash command.
#[derive(Debug, Clone, Copy)]
pub struct CommandDef {
    pub name: &'static str,
    pub description: &'static str,
}

const COMMANDS: &[CommandDef] = &[
    CommandDef { name: "/help",        description: "Show keybindings and commands" },
    CommandDef { name: "/models",      description: "Browse and switch models" },
    CommandDef { name: "/plan",        description: "Toggle Plan/Act mode" },
    CommandDef { name: "/theme",       description: "Cycle through built-in themes" },
    CommandDef { name: "/compact",     description: "Compact tool display (one line per tool)" },
    CommandDef { name: "/verbose",     description: "Verbose tool display (show output)" },
    CommandDef { name: "/grouped",     description: "Grouped tool display (bordered, no output)" },
    CommandDef { name: "/transcript",  description: "Open full conversation transcript" },
    CommandDef { name: "/clear",       description: "Clear conversation history" },
    CommandDef { name: "/exit",        description: "Exit Eve" },
];

// ── CommandAction ─────────────────────────────────────────────────────────

/// Result returned from [`CommandPopup::handle_key`].
#[derive(Debug, PartialEq, Eq)]
pub enum CommandAction {
    /// No action — the popup consumed the key.
    None,
    /// User confirmed a command; the full name (e.g. `"/exit"`) is returned.
    Select(String),
    /// User dismissed the popup (Escape or Backspace on empty query).
    Dismiss,
}

// ── CommandPopup ──────────────────────────────────────────────────────────

/// Floating slash-command chooser.
///
/// Renders a bordered list of commands above the input row. Commands are
/// filtered by a simple case-insensitive substring match against `query`.
pub struct CommandPopup {
    /// Characters typed after `/` (not including the `/` itself).
    query: String,
    /// All available commands.
    commands: Vec<CommandDef>,
    /// Indices into `commands` that match the current query.
    filtered: Vec<usize>,
    /// Index into `filtered` that is currently highlighted.
    selected: usize,
    /// Whether the popup is currently shown.
    visible: bool,
}

impl CommandPopup {
    /// Create a new (hidden) popup with the full command list.
    pub fn new() -> Self {
        let commands: Vec<CommandDef> = COMMANDS.to_vec();
        let filtered: Vec<usize> = (0..commands.len()).collect();
        Self {
            query: String::new(),
            commands,
            filtered,
            selected: 0,
            visible: false,
        }
    }

    // ── Visibility ────────────────────────────────────────────────────────

    /// Show the popup (resets query and re-filters).
    pub fn show(&mut self) {
        self.visible = true;
        self.query.clear();
        self.refilter();
    }

    /// Hide the popup.
    pub fn hide(&mut self) {
        self.visible = false;
    }

    /// Whether the popup is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    // ── Query / filtering ─────────────────────────────────────────────────

    /// Update the query string and re-filter the command list.
    ///
    /// Called by the owning widget when the text after `/` changes.
    pub fn update_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.refilter();
    }

    /// Recompute `filtered` and clamp `selected`.
    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .commands
            .iter()
            .enumerate()
            .filter(|(_, cmd)| cmd.name.to_lowercase().contains(&q))
            .map(|(i, _)| i)
            .collect();
        // Clamp selection so it stays in bounds.
        if self.filtered.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.filtered.len() - 1);
        }
    }

    // ── Key handling ──────────────────────────────────────────────────────

    /// Process a key event and return the resulting [`CommandAction`].
    ///
    /// Navigation (Up/Down), confirmation (Enter), dismissal (Escape /
    /// Backspace on empty query), and character input are all handled here.
    pub fn handle_key(&mut self, key: KeyEvent) -> CommandAction {
        match (key.code, key.modifiers) {
            // ── Navigate up ──────────────────────────────────────────────
            (KeyCode::Up, _) => {
                if !self.filtered.is_empty() && self.selected > 0 {
                    self.selected -= 1;
                }
                CommandAction::None
            }

            // ── Navigate down ────────────────────────────────────────────
            (KeyCode::Down, _) => {
                if !self.filtered.is_empty() && self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
                CommandAction::None
            }

            // ── Confirm selection ─────────────────────────────────────────
            (KeyCode::Enter, _) => {
                if let Some(&idx) = self.filtered.get(self.selected) {
                    let name = self.commands[idx].name.to_owned();
                    self.hide();
                    CommandAction::Select(name)
                } else {
                    self.hide();
                    CommandAction::Dismiss
                }
            }

            // ── Dismiss ──────────────────────────────────────────────────
            (KeyCode::Esc, _) => {
                self.hide();
                CommandAction::Dismiss
            }

            // ── Backspace ────────────────────────────────────────────────
            (KeyCode::Backspace, _) => {
                if self.query.is_empty() {
                    self.hide();
                    CommandAction::Dismiss
                } else {
                    // Remove last char from query.
                    let mut chars = self.query.chars();
                    chars.next_back();
                    self.query = chars.as_str().to_owned();
                    self.refilter();
                    CommandAction::None
                }
            }

            // ── Character input ───────────────────────────────────────────
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                self.query.push(c);
                self.refilter();
                CommandAction::None
            }

            _ => CommandAction::None,
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────

    /// Render the popup floating above the bottom of `area`.
    ///
    /// The popup is placed immediately above the last row of `area` (which
    /// is typically the input row). It occupies at most all available rows
    /// and is horizontally indented by 2 columns from the left.
    ///
    /// Nothing is drawn when the popup is hidden or there are no matches.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.visible || area.height == 0 || area.width == 0 {
            return;
        }

        let items = self.filtered.len();
        if items == 0 {
            return;
        }

        // Popup dimensions: 2 border rows + 1 row per item.
        let popup_height = (items as u16 + 2).min(area.height);
        // Width: up to the full area width minus a small indent, capped to 60.
        let popup_width = area.width.saturating_sub(4).clamp(20, 60);

        // Position: just above the last row of area.
        let popup_x = area.x + 2;
        let popup_y = area
            .y
            .saturating_add(area.height)
            .saturating_sub(1)           // above the input row
            .saturating_sub(popup_height);

        if popup_x + popup_width > area.x + area.width {
            return; // not enough horizontal space
        }

        // Shadow behind popup.
        let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
        crate::render::render_shadow(popup_rect, buf);

        let border_style = Style::default().fg(BORDER_FG);

        // Draw the full box border in one call.
        crate::render::draw_box(popup_rect, buf, border_style, None);

        // ── Item rows ─────────────────────────────────────────────────────
        let inner_width = popup_width.saturating_sub(2) as usize;

        // How many items can we actually render inside the border rows?
        let visible_items = (popup_height.saturating_sub(2)) as usize;

        // Scroll offset: keep `selected` visible.
        let scroll_offset = if self.selected >= visible_items {
            self.selected - visible_items + 1
        } else {
            0
        };

        for row_idx in 0..visible_items {
            let item_idx = row_idx + scroll_offset;
            if item_idx >= self.filtered.len() {
                break;
            }
            let cmd_idx = self.filtered[item_idx];
            let cmd = &self.commands[cmd_idx];
            let is_selected = item_idx == self.selected;

            let row_y = popup_y + 1 + row_idx as u16;

            // Build the row content as a Line and render it.
            let line = build_item_line(cmd, is_selected, inner_width);
            let mut x = popup_x + 1;
            for span in &line.spans {
                let style = span.style;
                for ch in span.content.chars() {
                    if x >= popup_x + popup_width - 1 {
                        break;
                    }
                    if let Some(cell) = buf.cell_mut((x, row_y)) {
                        let mut char_buf = [0u8; 4];
                        let s = ch.encode_utf8(&mut char_buf);
                        cell.set_symbol(s);
                        cell.set_style(style);
                    }
                    let char_width =
                        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1) as u16;
                    x += char_width;
                }
            }
            // Pad the rest of the inner area.
            let bg_style = if is_selected {
                Style::default()
                    .bg(SELECTED_BG)
                    .fg(SELECTED_TEXT_FG)
            } else {
                Style::default()
            };
            while x < popup_x + popup_width - 1 {
                if let Some(cell) = buf.cell_mut((x, row_y)) {
                    cell.set_symbol(" ");
                    cell.set_style(bg_style);
                }
                x += 1;
            }
        }
    }
}

impl Default for CommandPopup {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Build the styled [`Line`] for a single popup row.
///
/// Layout: `  /name      description…`
/// Selected row has a coloured background and the `>` indicator.
fn build_item_line(cmd: &CommandDef, is_selected: bool, inner_width: usize) -> Line<'static> {
    // Indicator: "> " for selected, "  " otherwise.
    let indicator = if is_selected { "> " } else { "  " };

    let indicator_style = if is_selected {
        Style::default()
            .fg(SELECTED_TEXT_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(INDICATOR_FG)
    };

    let name = cmd.name;
    // Fixed name column width (longest name "/personality" = 12 chars + leading slash = 12).
    // We reserve 14 chars for the name (padded with spaces).
    let name_col_width = 14usize;
    let name_padded = format!("{:<width$}", name, width = name_col_width);

    let name_style = if is_selected {
        Style::default()
            .fg(SELECTED_TEXT_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(NAME_FG)
    };

    let desc_style = if is_selected {
        Style::default().fg(SELECTED_TEXT_FG)
    } else {
        Style::default().fg(DESC_FG)
    };

    // Truncate description to remaining space.
    let used = indicator.len() + name_col_width;
    let desc_width = inner_width.saturating_sub(used);
    let desc = truncate_str(cmd.description, desc_width);

    Line::from(vec![
        Span::styled(indicator.to_owned(), indicator_style),
        Span::styled(name_padded, name_style),
        Span::styled(desc, desc_style),
    ])
}

/// Truncate a string to at most `max_chars` Unicode scalar values.
/// Appends `…` if the string was truncated.
fn truncate_str(s: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_owned()
    } else if max_chars <= 1 {
        "…".to_owned()
    } else {
        let truncated: String = s.chars().take(max_chars - 1).collect();
        format!("{}…", truncated)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ── Filtering ─────────────────────────────────────────────────────────

    #[test]
    fn empty_query_shows_all() {
        let mut popup = CommandPopup::new();
        popup.show();
        assert_eq!(popup.filtered.len(), COMMANDS.len());
    }

    #[test]
    fn filter_matches_substring() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.update_query("hel");
        // Only "/help" should match.
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.commands[popup.filtered[0]].name, "/help");
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.update_query("HEL");
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.commands[popup.filtered[0]].name, "/help");
    }

    #[test]
    fn filter_with_no_matches_gives_empty_list() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.update_query("zzznomatch");
        assert!(popup.filtered.is_empty());
    }

    // ── Navigation ────────────────────────────────────────────────────────

    #[test]
    fn arrow_keys_navigate() {
        let mut popup = CommandPopup::new();
        popup.show(); // selected = 0

        // Down moves to index 1.
        let action = popup.handle_key(key(KeyCode::Down));
        assert_eq!(action, CommandAction::None);
        assert_eq!(popup.selected, 1);

        // Down again to index 2.
        popup.handle_key(key(KeyCode::Down));
        assert_eq!(popup.selected, 2);

        // Up moves back to index 1.
        let action = popup.handle_key(key(KeyCode::Up));
        assert_eq!(action, CommandAction::None);
        assert_eq!(popup.selected, 1);
    }

    #[test]
    fn up_at_first_item_stays() {
        let mut popup = CommandPopup::new();
        popup.show(); // selected = 0
        popup.handle_key(key(KeyCode::Up));
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn down_at_last_item_stays() {
        let mut popup = CommandPopup::new();
        popup.show();
        // Move to last item.
        let last = popup.filtered.len() - 1;
        popup.selected = last;
        popup.handle_key(key(KeyCode::Down));
        assert_eq!(popup.selected, last);
    }

    // ── Confirm / dismiss ─────────────────────────────────────────────────

    #[test]
    fn enter_selects_command() {
        let mut popup = CommandPopup::new();
        popup.show();
        // First item is "/help" (safe default).
        let action = popup.handle_key(key(KeyCode::Enter));
        assert_eq!(action, CommandAction::Select("/help".to_owned()));
        assert!(!popup.is_visible());
    }

    #[test]
    fn escape_dismisses() {
        let mut popup = CommandPopup::new();
        popup.show();
        let action = popup.handle_key(key(KeyCode::Esc));
        assert_eq!(action, CommandAction::Dismiss);
        assert!(!popup.is_visible());
    }

    #[test]
    fn backspace_on_non_empty_query_removes_char() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.update_query("mo");
        let action = popup.handle_key(key(KeyCode::Backspace));
        assert_eq!(action, CommandAction::None);
        assert_eq!(popup.query, "m");
        assert!(popup.is_visible());
    }

    #[test]
    fn backspace_on_empty_query_dismisses() {
        let mut popup = CommandPopup::new();
        popup.show();
        // query is empty at show().
        let action = popup.handle_key(key(KeyCode::Backspace));
        assert_eq!(action, CommandAction::Dismiss);
        assert!(!popup.is_visible());
    }

    #[test]
    fn typing_chars_updates_query_and_refilters() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.handle_key(char_key('e'));
        popup.handle_key(char_key('x'));
        popup.handle_key(char_key('i'));
        popup.handle_key(char_key('t'));
        assert_eq!(popup.query, "exit");
        // Only "/exit" should match.
        assert_eq!(popup.filtered.len(), 1);
        assert_eq!(popup.commands[popup.filtered[0]].name, "/exit");
    }

    // ── Visibility ────────────────────────────────────────────────────────

    #[test]
    fn show_makes_visible_and_resets_query() {
        let mut popup = CommandPopup::new();
        popup.update_query("something");
        popup.show();
        assert!(popup.is_visible());
        assert_eq!(popup.query, "");
        assert_eq!(popup.filtered.len(), COMMANDS.len());
    }

    #[test]
    fn hide_makes_invisible() {
        let mut popup = CommandPopup::new();
        popup.show();
        popup.hide();
        assert!(!popup.is_visible());
    }

    // ── truncate_str helper ───────────────────────────────────────────────

    #[test]
    fn truncate_str_short_string_unchanged() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn truncate_str_exact_length_unchanged() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn truncate_str_long_string_gets_ellipsis() {
        let result = truncate_str("hello world", 8);
        assert!(result.ends_with('…'));
        // char count ≤ 8
        assert!(result.chars().count() <= 8);
    }

    #[test]
    fn truncate_str_zero_width_is_empty() {
        assert_eq!(truncate_str("hello", 0), "");
    }
}

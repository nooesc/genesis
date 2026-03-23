//! @-file completion popup — triggered when user types `@` in the input.
//!
//! Shows a floating popup above the input with fuzzy-filtered file paths
//! from the current working directory. Tab/Enter inserts the selected path.

use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
};

use crate::history::rgb;

const SELECTED_BG: Color = rgb(genesis_ui::colors::UI_ACCENT);
const NAME_FG: Color = rgb(genesis_ui::colors::UI_TEXT);
const PATH_FG: Color = rgb(genesis_ui::colors::UI_DIM);
const BORDER_FG: Color = rgb(genesis_ui::colors::UI_MUTED);
const SELECTED_TEXT_FG: Color = rgb(genesis_ui::colors::EVE_DARK);

/// Result from handling a key event.
#[derive(Debug, PartialEq, Eq)]
pub enum FileCompletionAction {
    /// No action — key consumed.
    None,
    /// User selected a file path to insert.
    Select(String),
    /// User dismissed the popup.
    Dismiss,
}

/// How long a cached file scan remains fresh before rescanning.
const SCAN_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Floating file completion popup.
pub struct FileCompletion {
    /// Characters typed after `@` (the search query).
    query: String,
    /// All discovered file paths (relative to cwd).
    all_files: Vec<String>,
    /// Indices into `all_files` matching the current query.
    filtered: Vec<usize>,
    /// Index into `filtered` for the highlighted entry.
    selected: usize,
    /// Whether the popup is visible.
    visible: bool,
    /// The directory used for the last file scan (for cache invalidation).
    last_scan_dir: Option<PathBuf>,
    /// When the last file scan completed (for TTL-based cache expiry).
    last_scan_time: Option<Instant>,
}

impl FileCompletion {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            all_files: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            visible: false,
            last_scan_dir: None,
            last_scan_time: None,
        }
    }

    /// Show the popup, scanning the cwd for files if the cache is stale.
    ///
    /// The file scan is skipped when the working directory has not changed and
    /// the previous scan is younger than [`SCAN_CACHE_TTL`]. This avoids
    /// blocking the async runtime with repeated `read_dir` syscalls each time
    /// the popup is opened.
    pub fn show(&mut self) {
        self.visible = true;
        self.query.clear();

        let cwd = std::env::current_dir().ok();
        let cache_fresh = match (&self.last_scan_dir, &self.last_scan_time, &cwd) {
            (Some(prev_dir), Some(prev_time), Some(cur_dir)) => {
                prev_dir == cur_dir && prev_time.elapsed() < SCAN_CACHE_TTL
            }
            _ => false,
        };

        if !cache_fresh {
            self.scan_files();
            self.last_scan_dir = cwd;
            self.last_scan_time = Some(Instant::now());
        }

        self.refilter();
    }

    pub fn hide(&mut self) {
        self.visible = false;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Update the query and refilter.
    pub fn update_query(&mut self, query: &str) {
        self.query = query.to_owned();
        self.refilter();
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) -> FileCompletionAction {
        match (key.code, key.modifiers) {
            // Navigate
            (KeyCode::Up, _) => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                FileCompletionAction::None
            }
            (KeyCode::Down, _) => {
                if !self.filtered.is_empty() && self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
                FileCompletionAction::None
            }

            // Select
            (KeyCode::Enter, _) | (KeyCode::Tab, _) => {
                if let Some(&idx) = self.filtered.get(self.selected) {
                    let path = self.all_files[idx].clone();
                    self.hide();
                    FileCompletionAction::Select(path)
                } else {
                    self.hide();
                    FileCompletionAction::Dismiss
                }
            }

            // Dismiss
            (KeyCode::Esc, _) => {
                self.hide();
                FileCompletionAction::Dismiss
            }

            // Backspace
            (KeyCode::Backspace, _) => {
                if self.query.is_empty() {
                    self.hide();
                    FileCompletionAction::Dismiss
                } else {
                    self.query.pop();
                    self.refilter();
                    FileCompletionAction::None
                }
            }

            // Character input
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                // Space dismisses the popup.
                if c == ' ' {
                    self.hide();
                    return FileCompletionAction::Dismiss;
                }
                self.query.push(c);
                self.refilter();
                FileCompletionAction::None
            }

            _ => FileCompletionAction::None,
        }
    }

    /// Render the popup floating above the bottom of `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if !self.visible || area.height == 0 || area.width == 0 {
            return;
        }

        let items = self.filtered.len();
        if items == 0 {
            return;
        }

        let popup_height = (items as u16 + 2).min(area.height).min(12);
        let popup_width = area.width.saturating_sub(4).clamp(20, 60);

        let popup_x = area.x + 2;
        let popup_y = area
            .y
            .saturating_add(area.height)
            .saturating_sub(1)
            .saturating_sub(popup_height);

        if popup_x + popup_width > area.x + area.width {
            return;
        }

        // Shadow behind popup.
        let popup_rect = Rect::new(popup_x, popup_y, popup_width, popup_height);
        crate::render::render_shadow(popup_rect, buf);

        let border_style = Style::default().fg(BORDER_FG);

        // Draw the full box border in one call.
        crate::render::draw_box(popup_rect, buf, border_style, None);

        // Overlay title on the top border.
        let title = " Files ";
        let mut col = popup_x + 1;
        for ch in title.chars() {
            if col >= popup_x + popup_width - 1 {
                break;
            }
            if let Some(cell) = buf.cell_mut((col, popup_y)) {
                cell.set_symbol(&ch.to_string());
                cell.set_style(Style::default().fg(PATH_FG));
            }
            col += 1;
        }

        // Item rows.
        let inner_width = popup_width.saturating_sub(2) as usize;
        let visible_items = (popup_height.saturating_sub(2)) as usize;
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
            let file_idx = self.filtered[item_idx];
            let path = &self.all_files[file_idx];
            let is_selected = item_idx == self.selected;

            let row_y = popup_y + 1 + row_idx as u16;

            // Content.
            let indicator = if is_selected { "▸ " } else { "  " };
            let display = if path.len() > inner_width.saturating_sub(2) {
                format!(
                    "{}…",
                    &path[..path.floor_char_boundary(inner_width.saturating_sub(3))]
                )
            } else {
                path.clone()
            };

            let style = if is_selected {
                Style::default()
                    .fg(SELECTED_TEXT_FG)
                    .bg(SELECTED_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(NAME_FG)
            };
            let bg_style = if is_selected {
                Style::default().bg(SELECTED_BG)
            } else {
                Style::default()
            };

            let mut x = popup_x + 1;
            for ch in indicator.chars().chain(display.chars()) {
                if x >= popup_x + popup_width - 1 {
                    break;
                }
                if let Some(cell) = buf.cell_mut((x, row_y)) {
                    cell.set_symbol(&ch.to_string());
                    cell.set_style(style);
                }
                x += 1;
            }
            // Pad remaining with bg.
            while x < popup_x + popup_width - 1 {
                if let Some(cell) = buf.cell_mut((x, row_y)) {
                    cell.set_symbol(" ");
                    cell.set_style(bg_style);
                }
                x += 1;
            }
        }
    }

    // ── Private ──────────────────────────────────────────────────────────

    /// Scan the current working directory for files (max 2 levels deep).
    fn scan_files(&mut self) {
        self.all_files.clear();
        let Ok(cwd) = std::env::current_dir() else {
            return;
        };

        // Collect files up to 2 levels deep, skip hidden dirs.
        self.walk_dir(&cwd, &cwd, 0, 2);
        self.all_files.sort();
    }

    fn walk_dir(
        &mut self,
        root: &std::path::Path,
        dir: &std::path::Path,
        depth: usize,
        max_depth: usize,
    ) {
        if depth > max_depth || self.all_files.len() > 500 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            // Skip hidden files/dirs and common noise.
            if name_str.starts_with('.') || name_str == "node_modules" || name_str == "target" {
                continue;
            }

            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(root) {
                self.all_files.push(rel.display().to_string());
            }

            if path.is_dir() {
                self.walk_dir(root, &path, depth + 1, max_depth);
            }
        }
    }

    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .all_files
            .iter()
            .enumerate()
            .filter(|(_, path)| {
                if q.is_empty() {
                    return true;
                }
                path.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();

        if self.filtered.is_empty() {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(self.filtered.len() - 1);
        }
    }
}

impl Default for FileCompletion {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_hidden() {
        let fc = FileCompletion::new();
        assert!(!fc.is_visible());
    }

    #[test]
    fn show_makes_visible() {
        let mut fc = FileCompletion::new();
        fc.show();
        assert!(fc.is_visible());
        // Should have discovered some files in the cwd.
        assert!(!fc.all_files.is_empty(), "should find files in cwd");
    }

    #[test]
    fn esc_dismisses() {
        let mut fc = FileCompletion::new();
        fc.show();
        let action = fc.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(action, FileCompletionAction::Dismiss);
        assert!(!fc.is_visible());
    }

    #[test]
    fn enter_selects() {
        let mut fc = FileCompletion::new();
        fc.show();
        if !fc.filtered.is_empty() {
            let action = fc.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
            assert!(matches!(action, FileCompletionAction::Select(_)));
        }
    }

    #[test]
    fn tab_selects() {
        let mut fc = FileCompletion::new();
        fc.show();
        if !fc.filtered.is_empty() {
            let action = fc.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
            assert!(matches!(action, FileCompletionAction::Select(_)));
        }
    }

    #[test]
    fn space_dismisses() {
        let mut fc = FileCompletion::new();
        fc.show();
        let action = fc.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(action, FileCompletionAction::Dismiss);
    }

    #[test]
    fn typing_filters() {
        let mut fc = FileCompletion::new();
        fc.show();
        let before = fc.filtered.len();
        // Type something specific to narrow results.
        fc.handle_key(KeyEvent::new(KeyCode::Char('C'), KeyModifiers::SHIFT));
        fc.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
        fc.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
        fc.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        fc.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        // Should have fewer results than before (or same if "Cargo" matches everything).
        assert!(fc.filtered.len() <= before);
    }
}

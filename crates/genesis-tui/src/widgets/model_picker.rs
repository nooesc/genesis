//! Model picker overlay — interactive model browser for OpenRouter.
//!
//! Shows a searchable, scrollable list of available models with pricing,
//! context length, and capability badges. Launched via `/models`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget as _},
};

use crate::history::rgb;
use genesis_provider::openrouter_models::OpenRouterModel;

const ACCENT: Color = rgb(genesis_ui::colors::EVE_LAVENDER);
const TEXT: Color = rgb(genesis_ui::colors::UI_TEXT);
const DIM: Color = rgb(genesis_ui::colors::UI_DIM);
const SUCCESS: Color = rgb(genesis_ui::colors::UI_SUCCESS);
const BORDER: Color = Color::Rgb(88, 88, 88);
const SELECTED_BG: Color = Color::Rgb(45, 42, 55);
const NEW_COLOR: Color = Color::Rgb(255, 175, 50);

/// Sort order for the model list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SortMode {
    #[default]
    Newest,
    Cheapest,
    LargestContext,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            SortMode::Newest => SortMode::Cheapest,
            SortMode::Cheapest => SortMode::LargestContext,
            SortMode::LargestContext => SortMode::Newest,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SortMode::Newest => "Newest",
            SortMode::Cheapest => "Cheapest",
            SortMode::LargestContext => "Largest Context",
        }
    }
}

/// Capability filter for the model list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CapabilityFilter {
    #[default]
    All,
    Tools,
    Vision,
    Reasoning,
}

impl CapabilityFilter {
    fn next(self) -> Self {
        match self {
            CapabilityFilter::All => CapabilityFilter::Tools,
            CapabilityFilter::Tools => CapabilityFilter::Vision,
            CapabilityFilter::Vision => CapabilityFilter::Reasoning,
            CapabilityFilter::Reasoning => CapabilityFilter::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            CapabilityFilter::All => "All",
            CapabilityFilter::Tools => "Tools",
            CapabilityFilter::Vision => "Vision",
            CapabilityFilter::Reasoning => "Reasoning",
        }
    }
}

/// Format a model's age from its creation timestamp.
/// Returns true if the model was created within the last 7 days.
fn is_new_model(created: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(created) < 7 * 86400
}

fn format_age(created: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age_secs = now.saturating_sub(created);
    let days = age_secs / 86400;

    if days < 7 {
        "new!".to_owned()
    } else if days < 30 {
        format!("{}d", days)
    } else if days < 365 {
        format!("{}mo", days / 30)
    } else {
        format!("{}y", days / 365)
    }
}

/// Result of handling a key in the model picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelPickerAction {
    None,
    /// User selected a model — ID to switch to.
    Select(String),
    /// User dismissed the picker.
    Close,
}

/// Interactive model picker overlay.
pub struct ModelPicker {
    /// All available models.
    all_models: Vec<OpenRouterModel>,
    /// Indices into `all_models` that match the current search.
    filtered: Vec<usize>,
    /// Current search query.
    query: String,
    /// Index into `filtered` that is highlighted.
    selected: usize,
    /// Scroll offset for the list.
    scroll: usize,
    /// Whether the picker is loading models.
    loading: bool,
    /// Error message if fetch failed.
    error: Option<String>,
    /// Current sort order.
    sort_mode: SortMode,
    /// Current capability filter.
    capability_filter: CapabilityFilter,
}

impl ModelPicker {
    /// Create a new picker in loading state.
    pub fn new_loading() -> Self {
        Self {
            all_models: Vec::new(),
            filtered: Vec::new(),
            query: String::new(),
            selected: 0,
            scroll: 0,
            loading: true,
            error: None,
            sort_mode: SortMode::Newest,
            capability_filter: CapabilityFilter::All,
        }
    }

    /// Set the model list (called when fetch completes).
    pub fn set_models(&mut self, models: Vec<OpenRouterModel>) {
        self.all_models = models;
        self.loading = false;
        self.refilter();
    }

    /// Set an error message (called when fetch fails).
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
        self.loading = false;
    }

    /// Handle a key event. `_visible_rows` is accepted for API consistency
    /// with other overlays but is not currently used.
    pub fn handle_key(&mut self, key: KeyEvent, _visible_rows: u16) -> ModelPickerAction {
        match (key.code, key.modifiers) {
            // Close
            (KeyCode::Esc, _) => ModelPickerAction::Close,

            // Select
            (KeyCode::Enter, _) => {
                if let Some(&idx) = self.filtered.get(self.selected) {
                    let model_id = self.all_models[idx].id.clone();
                    ModelPickerAction::Select(model_id)
                } else {
                    ModelPickerAction::None
                }
            }

            // Sort toggle
            (KeyCode::Tab, _) => {
                self.sort_mode = self.sort_mode.next();
                self.refilter();
                ModelPickerAction::None
            }

            // Capability filter toggle
            (KeyCode::Char('f'), mods) if mods.contains(KeyModifiers::CONTROL) => {
                self.capability_filter = self.capability_filter.next();
                self.refilter();
                ModelPickerAction::None
            }

            // Navigate
            (KeyCode::Up, _) => {
                if self.selected > 0 {
                    self.selected -= 1;
                    if self.selected < self.scroll {
                        self.scroll = self.selected;
                    }
                }
                ModelPickerAction::None
            }
            (KeyCode::Down, _) => {
                if !self.filtered.is_empty() && self.selected + 1 < self.filtered.len() {
                    self.selected += 1;
                }
                ModelPickerAction::None
            }

            // Search input
            (KeyCode::Char(c), mods)
                if mods == KeyModifiers::NONE || mods == KeyModifiers::SHIFT =>
            {
                self.query.push(c);
                self.refilter();
                ModelPickerAction::None
            }
            (KeyCode::Backspace, _) => {
                self.query.pop();
                self.refilter();
                ModelPickerAction::None
            }

            _ => ModelPickerAction::None,
        }
    }

    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.filtered = self
            .all_models
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                if q.is_empty() {
                    return true;
                }
                m.id.to_lowercase().contains(&q)
                    || m.name.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();

        // Apply capability filter.
        match self.capability_filter {
            CapabilityFilter::All => {}
            CapabilityFilter::Tools => {
                self.filtered.retain(|&i| self.all_models[i].supports_tools());
            }
            CapabilityFilter::Vision => {
                self.filtered.retain(|&i| self.all_models[i].supports_vision());
            }
            CapabilityFilter::Reasoning => {
                self.filtered.retain(|&i| self.all_models[i].supports_reasoning());
            }
        }

        // Sort filtered results.
        match self.sort_mode {
            SortMode::Newest => {
                self.filtered.sort_by(|&a, &b| {
                    self.all_models[b].created.cmp(&self.all_models[a].created)
                });
            }
            SortMode::Cheapest => {
                self.filtered.sort_by(|&a, &b| {
                    let price_a = self.all_models[a].pricing.prompt.parse::<f64>().unwrap_or(f64::MAX);
                    let price_b = self.all_models[b].pricing.prompt.parse::<f64>().unwrap_or(f64::MAX);
                    price_a.partial_cmp(&price_b).unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            SortMode::LargestContext => {
                self.filtered.sort_by(|&a, &b| {
                    self.all_models[b].context_length.cmp(&self.all_models[a].context_length)
                });
            }
        }

        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        // Reset scroll to top when results change — prevents hidden items
        // at the top after a sort/filter toggle.
        self.scroll = 0;
    }

    /// Render the picker overlay.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width < 30 || area.height < 8 {
            return;
        }

        // Full-screen overlay.
        let bg_style = Style::default().bg(Color::Rgb(20, 20, 25));
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(" ");
                    cell.set_style(bg_style);
                }
            }
        }

        // Header.
        let header = Line::from(vec![
            Span::styled(" Model Picker", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled("  -- ", Style::default().fg(DIM)),
            Span::styled(
                format!("{} models", self.filtered.len()),
                Style::default().fg(DIM),
            ),
            Span::styled("  Sort: ", Style::default().fg(DIM)),
            Span::styled(
                self.sort_mode.label(),
                Style::default().fg(ACCENT),
            ),
            Span::styled("  Filter: ", Style::default().fg(DIM)),
            Span::styled(
                self.capability_filter.label(),
                Style::default().fg(ACCENT),
            ),
        ]);
        let header_area = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
        Paragraph::new(header).render(header_area, buf);

        // Search bar.
        let search_y = area.y + 1;
        let search = Line::from(vec![
            Span::styled(" > ", Style::default().fg(DIM)),
            Span::styled(
                if self.query.is_empty() { "Type to search..." } else { &self.query },
                Style::default().fg(if self.query.is_empty() { DIM } else { TEXT }),
            ),
            Span::styled("|", Style::default().fg(ACCENT)),
        ]);
        let search_area = Rect { x: area.x, y: search_y, width: area.width, height: 1 };
        Paragraph::new(search).render(search_area, buf);

        // Separator.
        let sep_y = area.y + 2;
        let sep_style = Style::default().fg(BORDER);
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, sep_y)) {
                cell.set_symbol("─");
                cell.set_style(sep_style);
            }
        }

        // Loading / error state.
        if self.loading {
            let msg = Line::from(Span::styled(
                "  Loading models from OpenRouter...",
                Style::default().fg(DIM),
            ));
            let msg_area = Rect { x: area.x, y: area.y + 4, width: area.width, height: 1 };
            Paragraph::new(msg).render(msg_area, buf);
            return;
        }
        if let Some(ref err) = self.error {
            let msg = Line::from(Span::styled(
                format!("  Error: {err}"),
                Style::default().fg(Color::Red),
            ));
            let msg_area = Rect { x: area.x, y: area.y + 4, width: area.width, height: 1 };
            Paragraph::new(msg).render(msg_area, buf);
            return;
        }

        // Model list.
        let list_y = area.y + 3;
        let list_height = area.height.saturating_sub(4); // leave room for footer
        let visible = list_height as usize;

        // Ensure scroll keeps selected visible.
        let scroll = if self.selected >= self.scroll + visible {
            self.selected - visible + 1
        } else if self.selected < self.scroll {
            self.selected
        } else {
            self.scroll
        };

        for row in 0..visible {
            let item_idx = scroll + row;
            if item_idx >= self.filtered.len() {
                break;
            }
            let model_idx = self.filtered[item_idx];
            let model = &self.all_models[model_idx];
            let is_selected = item_idx == self.selected;

            let row_y = list_y + row as u16;
            let row_style = if is_selected {
                Style::default().bg(SELECTED_BG)
            } else {
                Style::default()
            };

            // Fill row background for selected.
            if is_selected {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, row_y)) {
                        cell.set_style(row_style);
                    }
                }
            }

            // Build model line.
            let mut spans: Vec<Span<'static>> = Vec::new();

            // Indicator.
            spans.push(Span::styled(
                if is_selected { " > " } else { "   " },
                Style::default().fg(ACCENT),
            ));

            // Model ID.
            let id_style = if is_selected {
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD).bg(SELECTED_BG)
            } else {
                Style::default().fg(TEXT)
            };
            // Truncate ID to fit.
            let max_id_len = (area.width as usize).saturating_sub(50).min(40);
            let id_display = if model.id.len() > max_id_len {
                format!("{}...", &model.id[..model.id.floor_char_boundary(max_id_len)])
            } else {
                format!("{:<width$}", model.id, width = max_id_len)
            };
            spans.push(Span::styled(id_display, id_style));

            // Price.
            let price = model.price_display();
            spans.push(Span::styled(
                format!("  {:<12}", price),
                Style::default().fg(DIM).bg(if is_selected { SELECTED_BG } else { Color::Reset }),
            ));

            // Context.
            spans.push(Span::styled(
                format!("{:>5}", model.context_display()),
                Style::default().fg(DIM).bg(if is_selected { SELECTED_BG } else { Color::Reset }),
            ));

            // Capability badges.
            let badge_bg = if is_selected { SELECTED_BG } else { Color::Reset };
            if model.supports_tools() {
                spans.push(Span::styled(" T", Style::default().fg(SUCCESS).bg(badge_bg)));
            }
            if model.supports_vision() {
                spans.push(Span::styled(" V", Style::default().fg(Color::Cyan).bg(badge_bg)));
            }
            if model.supports_reasoning() {
                spans.push(Span::styled(" R", Style::default().fg(Color::Yellow).bg(badge_bg)));
            }

            // Age indicator.
            let age = format_age(model.created);
            let age_style = if is_new_model(model.created) {
                Style::default().fg(NEW_COLOR).bg(badge_bg)
            } else {
                Style::default().fg(DIM).bg(badge_bg)
            };
            spans.push(Span::styled(format!(" {age}"), age_style));

            let line = Line::from(spans);
            let line_area = Rect { x: area.x, y: row_y, width: area.width, height: 1 };
            Paragraph::new(line).render(line_area, buf);
        }

        // Footer.
        let footer_y = area.y + area.height - 1;
        let footer = Line::from(vec![
            Span::styled(" Enter", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(" select  ", Style::default().fg(DIM)),
            Span::styled("Tab", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(" sort  ", Style::default().fg(DIM)),
            Span::styled("Ctrl+F", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(" filter  ", Style::default().fg(DIM)),
            Span::styled("Esc", Style::default().fg(ACCENT)),
            Span::styled(" close  ", Style::default().fg(DIM)),
            Span::styled("T", Style::default().fg(SUCCESS)),
            Span::styled("ools ", Style::default().fg(DIM)),
            Span::styled("V", Style::default().fg(Color::Cyan)),
            Span::styled("ision ", Style::default().fg(DIM)),
            Span::styled("R", Style::default().fg(Color::Yellow)),
            Span::styled("easoning", Style::default().fg(DIM)),
        ]);
        let footer_area = Rect { x: area.x, y: footer_y, width: area.width, height: 1 };
        Paragraph::new(footer).render(footer_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genesis_provider::openrouter_models::{ModelArchitecture, ModelPricing};

    fn make_models() -> Vec<OpenRouterModel> {
        vec![
            OpenRouterModel {
                id: "openai/gpt-4.1".to_owned(),
                name: "GPT-4.1".to_owned(),
                created: 1000,
                context_length: 1_048_576,
                pricing: ModelPricing { prompt: "0.000002".to_owned(), completion: "0.00001".to_owned() },
                architecture: ModelArchitecture::default(),
                supported_parameters: vec!["tools".to_owned()],
            },
            OpenRouterModel {
                id: "anthropic/claude-sonnet-4-6".to_owned(),
                name: "Claude Sonnet 4.6".to_owned(),
                created: 2000,
                context_length: 200_000,
                pricing: ModelPricing { prompt: "0.000003".to_owned(), completion: "0.000015".to_owned() },
                architecture: ModelArchitecture { input_modalities: vec!["text".to_owned(), "image".to_owned()], output_modalities: vec!["text".to_owned()] },
                supported_parameters: vec!["tools".to_owned(), "reasoning".to_owned()],
            },
        ]
    }

    #[test]
    fn search_filters_models() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        assert_eq!(picker.filtered.len(), 2);

        picker.query = "claude".to_owned();
        picker.refilter();
        assert_eq!(picker.filtered.len(), 1);
        assert_eq!(picker.all_models[picker.filtered[0]].id, "anthropic/claude-sonnet-4-6");
    }

    #[test]
    fn esc_closes() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        let action = picker.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::Close);
    }

    #[test]
    fn enter_selects_model() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        // Default sort is Newest, so claude (created=2000) should be first.
        assert_eq!(action, ModelPickerAction::Select("anthropic/claude-sonnet-4-6".to_owned()));
    }

    #[test]
    fn navigate_and_select() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        picker.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 24);
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        // Second item in Newest order (created=1000) is gpt-4.1.
        assert_eq!(action, ModelPickerAction::Select("openai/gpt-4.1".to_owned()));
    }

    #[test]
    fn sort_mode_cycles() {
        assert_eq!(SortMode::Newest.next(), SortMode::Cheapest);
        assert_eq!(SortMode::Cheapest.next(), SortMode::LargestContext);
        assert_eq!(SortMode::LargestContext.next(), SortMode::Newest);
    }

    #[test]
    fn capability_filter_cycles() {
        assert_eq!(CapabilityFilter::All.next(), CapabilityFilter::Tools);
        assert_eq!(CapabilityFilter::Tools.next(), CapabilityFilter::Vision);
        assert_eq!(CapabilityFilter::Vision.next(), CapabilityFilter::Reasoning);
        assert_eq!(CapabilityFilter::Reasoning.next(), CapabilityFilter::All);
    }

    #[test]
    fn tab_cycles_sort_mode() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(vec![]);
        let action = picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::None);
        // Sort mode should have advanced from default.
        assert_eq!(picker.sort_mode, SortMode::Cheapest);
    }

    #[test]
    fn ctrl_f_cycles_filter() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(vec![]);
        let action = picker.handle_key(
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            24,
        );
        assert_eq!(action, ModelPickerAction::None);
        assert_eq!(picker.capability_filter, CapabilityFilter::Tools);
    }

    #[test]
    fn format_age_new_model() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(format_age(now), "new!");
        assert_eq!(format_age(now - 86400 * 3), "new!");
    }

    #[test]
    fn format_age_old_model() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let age = format_age(now - 86400 * 45);
        assert!(age.contains("mo"), "45 days should show months: {age}");
    }

    #[test]
    fn sort_cheapest_orders_by_price() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        // Switch to cheapest sort.
        picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 24);
        assert_eq!(picker.sort_mode, SortMode::Cheapest);
        // gpt-4.1 has prompt price 0.000002, claude has 0.000003 — gpt first.
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::Select("openai/gpt-4.1".to_owned()));
    }

    #[test]
    fn sort_largest_context_orders_by_context() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        // Tab twice: Newest -> Cheapest -> LargestContext.
        picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 24);
        picker.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), 24);
        assert_eq!(picker.sort_mode, SortMode::LargestContext);
        // gpt-4.1 has 1M context, claude has 200k — gpt first.
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::Select("openai/gpt-4.1".to_owned()));
    }

    #[test]
    fn filter_vision_only() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        // Ctrl+F twice: All -> Tools -> Vision.
        picker.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 24);
        picker.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 24);
        assert_eq!(picker.capability_filter, CapabilityFilter::Vision);
        // Only claude supports vision.
        assert_eq!(picker.filtered.len(), 1);
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::Select("anthropic/claude-sonnet-4-6".to_owned()));
    }

    #[test]
    fn filter_reasoning_only() {
        let mut picker = ModelPicker::new_loading();
        picker.set_models(make_models());
        // Ctrl+F three times: All -> Tools -> Vision -> Reasoning.
        picker.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 24);
        picker.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 24);
        picker.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL), 24);
        assert_eq!(picker.capability_filter, CapabilityFilter::Reasoning);
        // Only claude supports reasoning.
        assert_eq!(picker.filtered.len(), 1);
        let action = picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(action, ModelPickerAction::Select("anthropic/claude-sonnet-4-6".to_owned()));
    }
}

//! Renderable trait + composition primitives for content-aware layout.
//!
//! Inspired by the Codex CLI pattern. Provides a standard interface for
//! widgets to declare their height given a width, enabling dynamic layouts.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget as _};
use unicode_width::UnicodeWidthStr;

/// Content-aware rendering trait.
///
/// Unlike ratatui's `Widget` which only provides `render(area, buf)`,
/// `Renderable` also declares `desired_height(width)` so parent layouts
/// can allocate space dynamically.
pub trait Renderable {
    /// Render into the given area.
    fn render(&self, area: Rect, buf: &mut Buffer);

    /// How many rows this content needs at the given width.
    fn desired_height(&self, width: u16) -> u16;

    /// Optional cursor position within the area (for input widgets).
    fn cursor_pos(&self, _area: Rect) -> Option<(u16, u16)> {
        None
    }
}

// ── Blanket implementations ─────────────────────────────────────────────

impl Renderable for Line<'static> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.clone()).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let w = width.max(1) as usize;
        let line_width: usize = self.spans.iter().map(|s| s.content.width()).sum();
        if line_width == 0 {
            1
        } else {
            ((line_width.saturating_sub(1)) / w + 1) as u16
        }
    }
}

impl Renderable for &str {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(*self).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let w = width.max(1) as usize;
        let text_width = self.width();
        if text_width == 0 {
            1
        } else {
            ((text_width.saturating_sub(1)) / w + 1) as u16
        }
    }
}

impl Renderable for String {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.as_str()).render(area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_str().desired_height(width)
    }
}

impl<R: Renderable> Renderable for Option<R> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if let Some(inner) = self {
            inner.render(area, buf);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.as_ref().map(|r| r.desired_height(width)).unwrap_or(0)
    }
}

/// Empty renderable — renders nothing, zero height.
impl Renderable for () {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}
    fn desired_height(&self, _width: u16) -> u16 {
        0
    }
}

// ── Composition primitives ──────────────────────────────────────────────

/// Vertical stack — renders children top-to-bottom, summing heights.
pub struct Column {
    children: Vec<Box<dyn Renderable>>,
}

impl Column {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
        }
    }

    pub fn push(mut self, child: impl Renderable + 'static) -> Self {
        self.children.push(Box::new(child));
        self
    }

    pub fn len(&self) -> usize {
        self.children.len()
    }

    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }
}

impl Default for Column {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for Column {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut y = area.y;
        for child in &self.children {
            let h = child.desired_height(area.width);
            if y + h > area.y + area.height {
                break; // No more space
            }
            let child_area = Rect {
                x: area.x,
                y,
                width: area.width,
                height: h,
            };
            child.render(child_area, buf);
            y += h;
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        self.children
            .iter()
            .map(|c| c.desired_height(width))
            .sum::<u16>()
    }
}

/// Inset wrapper — adds padding around a child.
pub struct Inset {
    child: Box<dyn Renderable>,
    top: u16,
    right: u16,
    bottom: u16,
    left: u16,
}

impl Inset {
    /// Uniform padding on all sides.
    pub fn uniform(padding: u16, child: impl Renderable + 'static) -> Self {
        Self {
            child: Box::new(child),
            top: padding,
            right: padding,
            bottom: padding,
            left: padding,
        }
    }

    /// Horizontal and vertical padding.
    pub fn symmetric(horizontal: u16, vertical: u16, child: impl Renderable + 'static) -> Self {
        Self {
            child: Box::new(child),
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }
}

impl Renderable for Inset {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let inner = Rect {
            x: area.x.saturating_add(self.left),
            y: area.y.saturating_add(self.top),
            width: area.width.saturating_sub(self.left + self.right),
            height: area.height.saturating_sub(self.top + self.bottom),
        };
        if inner.width > 0 && inner.height > 0 {
            self.child.render(inner, buf);
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        let inner_width = width.saturating_sub(self.left + self.right);
        self.child.desired_height(inner_width) + self.top + self.bottom
    }
}

/// Flex item for proportional sizing.
pub struct FlexItem {
    pub child: Box<dyn Renderable>,
    pub flex: u16, // weight (higher = more space)
}

/// Flex column — divides vertical space proportionally among children.
///
/// Each child gets `(flex / total_flex) * available_height` rows.
/// Children with `flex = 0` get their `desired_height` (fixed).
pub struct FlexColumn {
    items: Vec<FlexItem>,
}

impl FlexColumn {
    pub fn new() -> Self {
        Self { items: Vec::new() }
    }

    /// Add a fixed-height child (gets its desired_height).
    pub fn fixed(mut self, child: impl Renderable + 'static) -> Self {
        self.items.push(FlexItem {
            child: Box::new(child),
            flex: 0,
        });
        self
    }

    /// Add a flexible child with the given weight.
    pub fn flex(mut self, weight: u16, child: impl Renderable + 'static) -> Self {
        self.items.push(FlexItem {
            child: Box::new(child),
            flex: weight,
        });
        self
    }
}

impl Default for FlexColumn {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderable for FlexColumn {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let width = area.width;
        let total_height = area.height;

        // First pass: allocate fixed items
        let mut fixed_total: u16 = 0;
        let mut flex_total: u16 = 0;
        for item in &self.items {
            if item.flex == 0 {
                fixed_total += item.child.desired_height(width);
            } else {
                flex_total += item.flex;
            }
        }

        let remaining = total_height.saturating_sub(fixed_total);

        // Second pass: render
        let mut y = area.y;
        for item in &self.items {
            let h = if item.flex == 0 {
                item.child.desired_height(width)
            } else if flex_total > 0 {
                (remaining as u32 * item.flex as u32 / flex_total as u32) as u16
            } else {
                0
            };

            if h == 0 || y >= area.y + area.height {
                continue;
            }

            let child_area = Rect {
                x: area.x,
                y,
                width,
                height: h.min(area.y + area.height - y),
            };
            item.child.render(child_area, buf);
            y += h;
        }
    }

    fn desired_height(&self, width: u16) -> u16 {
        // Desired height = sum of fixed items' heights + flex items' desired heights
        self.items
            .iter()
            .map(|item| item.child.desired_height(width))
            .sum()
    }
}

// ── Styled separator ────────────────────────────────────────────────────

/// A horizontal line separator (1 row).
pub struct Separator {
    style: Style,
}

impl Separator {
    pub fn new(style: Style) -> Self {
        Self { style }
    }
}

impl Renderable for Separator {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell_mut((x, area.y)) {
                cell.set_symbol("─");
                cell.set_style(self.style);
            }
        }
    }

    fn desired_height(&self, _width: u16) -> u16 {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_has_zero_height() {
        assert_eq!(().desired_height(80), 0);
    }

    #[test]
    fn str_height_single_line() {
        assert_eq!("hello".desired_height(80), 1);
    }

    #[test]
    fn str_height_wraps() {
        // 10 chars at width 5 = 2 lines
        assert_eq!("1234567890".desired_height(5), 2);
    }

    #[test]
    fn option_none_zero_height() {
        let opt: Option<String> = None;
        assert_eq!(opt.desired_height(80), 0);
    }

    #[test]
    fn option_some_delegates() {
        let opt = Some("hello".to_owned());
        assert_eq!(opt.desired_height(80), 1);
    }

    #[test]
    fn column_sums_heights() {
        let col = Column::new()
            .push("line 1".to_owned())
            .push("line 2".to_owned())
            .push("line 3".to_owned());
        assert_eq!(col.desired_height(80), 3);
    }

    #[test]
    fn column_renders_top_to_bottom() {
        let col = Column::new()
            .push("A".to_owned())
            .push("B".to_owned());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 2));
        col.render(Rect::new(0, 0, 10, 2), &mut buf);
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "A");
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "B");
    }

    #[test]
    fn inset_adds_padding() {
        let inset = Inset::uniform(1, "X".to_owned());
        // 1 line + 1 top + 1 bottom = 3
        assert_eq!(inset.desired_height(80), 3);
    }

    #[test]
    fn inset_renders_with_offset() {
        let inset = Inset::symmetric(2, 1, "X".to_owned());
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        inset.render(Rect::new(0, 0, 10, 3), &mut buf);
        // X should be at (2, 1) due to left=2, top=1
        assert_eq!(buf.cell((2, 1)).unwrap().symbol(), "X");
    }

    #[test]
    fn separator_is_one_row() {
        let sep = Separator::new(Style::default());
        assert_eq!(sep.desired_height(80), 1);
    }

    #[test]
    fn flex_column_fixed_items() {
        let flex = FlexColumn::new()
            .fixed("A".to_owned())
            .fixed("B".to_owned());
        assert_eq!(flex.desired_height(80), 2);
    }

    #[test]
    fn flex_column_renders_proportional() {
        let flex = FlexColumn::new()
            .fixed("header".to_owned())
            .flex(1, "content".to_owned());
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        flex.render(Rect::new(0, 0, 20, 10), &mut buf);
        // header at y=0, content gets remaining space
        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "h");
    }

    #[test]
    fn empty_column() {
        let col = Column::new();
        assert_eq!(col.desired_height(80), 0);
        assert!(col.is_empty());
    }
}

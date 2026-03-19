//! Popup shadow effect — renders a subtle shadow behind overlay widgets.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// Shadow offset (right and down).
const OFFSET: u16 = 1;
/// Shadow color.
const SHADOW_FG: Color = Color::Rgb(30, 30, 35);
const SHADOW_BG: Color = Color::Rgb(15, 15, 18);

/// Render a shadow behind an overlay at the given area.
/// Call this BEFORE rendering the overlay itself.
pub fn render_shadow(area: Rect, buf: &mut Buffer) {
    let shadow_style = Style::default().fg(SHADOW_FG).bg(SHADOW_BG);

    // Right edge shadow (1 column wide, full height).
    let right_x = area.x + area.width;
    if right_x < buf.area().width {
        for y in (area.y + OFFSET)..=(area.y + area.height).min(buf.area().height.saturating_sub(1))
        {
            if let Some(cell) = buf.cell_mut((right_x, y)) {
                cell.set_symbol("\u{2591}");
                cell.set_style(shadow_style);
            }
        }
    }

    // Bottom edge shadow (full width, 1 row tall).
    let bottom_y = area.y + area.height;
    if bottom_y < buf.area().height {
        for x in (area.x + OFFSET)..=(area.x + area.width).min(buf.area().width.saturating_sub(1)) {
            if let Some(cell) = buf.cell_mut((x, bottom_y)) {
                cell.set_symbol("\u{2591}");
                cell.set_style(shadow_style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    #[test]
    fn shadow_renders_at_offset() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 10));
        let popup_area = Rect::new(2, 2, 8, 4);
        render_shadow(popup_area, &mut buf);

        // Right edge shadow (x=10, y=3..6)
        let cell = buf.cell((10, 3)).unwrap();
        assert_eq!(cell.symbol(), "\u{2591}");

        // Bottom edge shadow (y=6, x=3..10)
        let cell = buf.cell((3, 6)).unwrap();
        assert_eq!(cell.symbol(), "\u{2591}");
    }

    #[test]
    fn shadow_clipped_at_buffer_edge() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        let popup_area = Rect::new(5, 2, 5, 3);
        // Shadow would extend beyond buffer — should not panic
        render_shadow(popup_area, &mut buf);
    }
}

//! Shared box-drawing helper — replaces duplicated cell-by-cell border code.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

/// Draw a box border around the given area using Unicode box-drawing characters.
///
/// Renders: `┌─┐` top, `│ │` sides, `└─┘` bottom.
/// Optionally fills the interior with a background style.
pub fn draw_box(area: Rect, buf: &mut Buffer, border_style: Style, fill_style: Option<Style>) {
    if area.width < 2 || area.height < 2 {
        return;
    }

    let right = area.x + area.width - 1;
    let bottom = area.y + area.height - 1;

    // Fill background if requested.
    if let Some(bg) = fill_style {
        for y in area.y..=bottom {
            for x in area.x..=right {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_symbol(" ");
                    cell.set_style(bg);
                }
            }
        }
    }

    // Top border: ┌───┐
    if let Some(cell) = buf.cell_mut((area.x, area.y)) {
        cell.set_symbol("┌");
        cell.set_style(border_style);
    }
    for x in area.x + 1..right {
        if let Some(cell) = buf.cell_mut((x, area.y)) {
            cell.set_symbol("─");
            cell.set_style(border_style);
        }
    }
    if let Some(cell) = buf.cell_mut((right, area.y)) {
        cell.set_symbol("┐");
        cell.set_style(border_style);
    }

    // Bottom border: └───┘
    if let Some(cell) = buf.cell_mut((area.x, bottom)) {
        cell.set_symbol("└");
        cell.set_style(border_style);
    }
    for x in area.x + 1..right {
        if let Some(cell) = buf.cell_mut((x, bottom)) {
            cell.set_symbol("─");
            cell.set_style(border_style);
        }
    }
    if let Some(cell) = buf.cell_mut((right, bottom)) {
        cell.set_symbol("┘");
        cell.set_style(border_style);
    }

    // Side borders: │ │
    for y in area.y + 1..bottom {
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol("│");
            cell.set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((right, y)) {
            cell.set_symbol("│");
            cell.set_style(border_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_box_renders_corners() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        draw_box(
            Rect::new(1, 1, 6, 3),
            &mut buf,
            Style::default(),
            None,
        );
        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "┌");
        assert_eq!(buf.cell((6, 1)).unwrap().symbol(), "┐");
        assert_eq!(buf.cell((1, 3)).unwrap().symbol(), "└");
        assert_eq!(buf.cell((6, 3)).unwrap().symbol(), "┘");
    }

    #[test]
    fn draw_box_renders_edges() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 5));
        draw_box(Rect::new(0, 0, 5, 3), &mut buf, Style::default(), None);
        // Top edge
        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), "─");
        assert_eq!(buf.cell((2, 0)).unwrap().symbol(), "─");
        // Side edge
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "│");
        assert_eq!(buf.cell((4, 1)).unwrap().symbol(), "│");
    }

    #[test]
    fn draw_box_too_small_no_crash() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 5));
        // 1x1 and 0x0 should not crash
        draw_box(Rect::new(0, 0, 1, 1), &mut buf, Style::default(), None);
        draw_box(Rect::new(0, 0, 0, 0), &mut buf, Style::default(), None);
    }

    #[test]
    fn draw_box_with_fill() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 4));
        let fill = Style::default().bg(ratatui::style::Color::Blue);
        draw_box(Rect::new(0, 0, 6, 4), &mut buf, Style::default(), Some(fill));
        // Interior should have fill style applied
        let interior = buf.cell((2, 1)).unwrap();
        assert_eq!(interior.symbol(), " ");
        assert_eq!(interior.bg, ratatui::style::Color::Blue);
    }
}

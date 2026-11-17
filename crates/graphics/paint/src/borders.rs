//! Border painting decisions.

use browser_css::Color;
use browser_layout::{Edges, Rect};
use browser_style::BorderStyle;

use crate::display_list::DisplayCommand;

/// Builds a border command, or `None` when no side is painted.
pub fn border_command(
    rect: Rect,
    widths: Edges,
    colors: [Color; 4],
    styles: [BorderStyle; 4],
) -> Option<DisplayCommand> {
    if rect.is_empty() {
        return None;
    }
    let any = styles
        .iter()
        .enumerate()
        .any(|(i, style)| style.is_visible() && side_width(&widths, i) > 0.0);
    if !any {
        return None;
    }
    Some(DisplayCommand::DrawBorder {
        rect,
        widths,
        colors,
        styles,
    })
}

/// Width of the side at `index` (top, right, bottom, left).
pub fn side_width(widths: &Edges, index: usize) -> f32 {
    match index {
        0 => widths.top,
        1 => widths.right,
        2 => widths.bottom,
        _ => widths.left,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_borders_without_visible_style() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(
            border_command(
                rect,
                Edges::uniform(1.0),
                [Color::rgb(0, 0, 0); 4],
                [BorderStyle::None; 4]
            )
            .is_none()
        );
        assert!(
            border_command(
                rect,
                Edges::uniform(1.0),
                [Color::rgb(0, 0, 0); 4],
                [BorderStyle::Solid; 4]
            )
            .is_some()
        );
    }

    #[test]
    fn skips_zero_width_borders() {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert!(
            border_command(
                rect,
                Edges::uniform(0.0),
                [Color::rgb(0, 0, 0); 4],
                [BorderStyle::Solid; 4]
            )
            .is_none()
        );
    }
}

//! Border painting decisions.

use browser_css::Color;
use browser_layout::{Edges, Rect};
use browser_style::BorderStyle;

use crate::display_list::DisplayCommand;
use crate::effects::OutlineData;

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

/// Builds an `outline` command as a border ring drawn outside the box.
///
/// The border is drawn on a rect inflated by `outline.offset + outline.width`, so
/// the resulting ring starts exactly `outline-offset` pixels outside the border box.
/// This is executable with the current `DrawBorder` primitive; the only known gap is
/// that `dashed`/`dotted`/`double` outlines use the rasterizer's approximate dash
/// pattern and that the offset gap shows whatever is painted behind the element.
pub fn outline_command(rect: Rect, outline: &OutlineData) -> Option<DisplayCommand> {
    if rect.is_empty() || outline.width <= 0.0 || !outline.style.is_visible() {
        return None;
    }
    Some(DisplayCommand::DrawBorder {
        rect: rect.inflate(outline.offset + outline.width),
        widths: Edges::uniform(outline.width),
        colors: [outline.color; 4],
        styles: [outline.style; 4],
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

    #[test]
    fn outline_command_inflates_by_offset_and_width() {
        let rect = Rect::new(5.0, 5.0, 10.0, 10.0);
        let outline = OutlineData {
            width: 2.0,
            style: BorderStyle::Solid,
            color: Color::rgb(255, 0, 0),
            offset: 3.0,
        };
        let Some(DisplayCommand::DrawBorder {
            rect: outer,
            widths,
            ..
        }) = outline_command(rect, &outline)
        else {
            panic!("expected a DrawBorder command");
        };
        assert_eq!(outer, rect.inflate(5.0));
        assert_eq!(widths, Edges::uniform(2.0));
        assert!(outline_command(rect, &OutlineData::default()).is_none());
    }
}

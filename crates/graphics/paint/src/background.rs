//! Background painting decisions.

use browser_css::Color;
use browser_layout::Rect;

use crate::display_list::DisplayCommand;

/// Builds the fill command for a box background, skipping fully transparent ones.
pub fn background_command(rect: Rect, color: Color) -> Option<DisplayCommand> {
    if color.a == 0 || rect.is_empty() {
        return None;
    }
    Some(DisplayCommand::FillRect { rect, color })
}

/// Composites a translucent colour over an opaque backdrop (useful for previews).
pub fn blend_over(foreground: Color, backdrop: Color) -> Color {
    let alpha = f32::from(foreground.a) / 255.0;
    let mix = |f: u8, b: u8| (f32::from(f) * alpha + f32::from(b) * (1.0 - alpha)).round() as u8;
    Color::rgba(
        mix(foreground.r, backdrop.r),
        mix(foreground.g, backdrop.g),
        mix(foreground.b, backdrop.b),
        255,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_transparent_backgrounds() {
        assert!(
            background_command(Rect::new(0.0, 0.0, 10.0, 10.0), Color::transparent()).is_none()
        );
        assert!(background_command(Rect::new(0.0, 0.0, 10.0, 10.0), Color::rgb(1, 2, 3)).is_some());
    }

    #[test]
    fn blends_colours() {
        let blended = blend_over(Color::rgba(255, 0, 0, 128), Color::rgb(0, 0, 255));
        assert!(blended.r > 100 && blended.b > 100);
        assert_eq!(blended.a, 255);
    }
}

//! Background painting decisions and `background-size` / `background-position` fitting.

use browser_css::Color;
use browser_layout::Rect;

use crate::display_list::DisplayCommand;
use crate::images::{FittedImage, ImagePosition, LengthOrPercentage};

/// The CSS `background-position` of a background image.
pub type BackgroundPosition = ImagePosition;

/// The CSS `background-size` of a background image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundSize {
    /// `cover`: scale preserving aspect ratio until the area is covered.
    Cover,
    /// `contain`: scale preserving aspect ratio until the image fits inside.
    Contain,
    /// Explicit width and/or height. `None` means `auto` for that component,
    /// in which case the missing dimension is derived from the intrinsic ratio.
    Explicit {
        /// Explicit width (`None` = `auto`).
        width: Option<LengthOrPercentage>,
        /// Explicit height (`None` = `auto`).
        height: Option<LengthOrPercentage>,
    },
}

impl Default for BackgroundSize {
    fn default() -> Self {
        Self::Explicit {
            width: None,
            height: None,
        }
    }
}

/// Resolves a size component against the corresponding positioning-area length.
///
/// Unlike [`LengthOrPercentage::resolve`] (which interprets negative lengths as
/// measured from the far edge for *position*), negative sizes are invalid CSS and
/// are simply passed through here.
fn resolve_size(value: LengthOrPercentage, reference: f32) -> f32 {
    match value {
        LengthOrPercentage::Percentage(fraction) => fraction * reference,
        LengthOrPercentage::Length(length) => length,
    }
}

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

/// Fits a raster background image with intrinsic size into a positioning area
/// according to CSS `background-size` / `background-position`.
///
/// The returned [`FittedImage::source`] is the whole image; sizing, aspect
/// handling and positioning are expressed through [`FittedImage::destination`].
/// As with [`crate::images::fit_image`], the destination can extend beyond the
/// area (for `cover` or oversized images) and must be clipped when painting.
pub fn fit_background(
    area: Rect,
    intrinsic_width: u32,
    intrinsic_height: u32,
    size: BackgroundSize,
    position: BackgroundPosition,
) -> FittedImage {
    if area.is_empty() || intrinsic_width == 0 || intrinsic_height == 0 {
        return FittedImage::empty(area);
    }

    let intrinsic_w = intrinsic_width as f32;
    let intrinsic_h = intrinsic_height as f32;
    let source = Rect::new(0.0, 0.0, intrinsic_w, intrinsic_h);

    let (draw_width, draw_height) = match size {
        BackgroundSize::Cover => {
            let scale = (area.width / intrinsic_w).max(area.height / intrinsic_h);
            (intrinsic_w * scale, intrinsic_h * scale)
        }
        BackgroundSize::Contain => {
            let scale = (area.width / intrinsic_w).min(area.height / intrinsic_h);
            (intrinsic_w * scale, intrinsic_h * scale)
        }
        BackgroundSize::Explicit { width, height } => match (width, height) {
            (Some(width), Some(height)) => (
                resolve_size(width, area.width),
                resolve_size(height, area.height),
            ),
            (Some(width), None) => {
                let draw_width = resolve_size(width, area.width);
                (draw_width, draw_width * intrinsic_h / intrinsic_w)
            }
            (None, Some(height)) => {
                let draw_height = resolve_size(height, area.height);
                (draw_height * intrinsic_w / intrinsic_h, draw_height)
            }
            (None, None) => (intrinsic_w, intrinsic_h),
        },
    };

    let destination = Rect::new(
        area.x + position.x.resolve(area.width - draw_width),
        area.y + position.y.resolve(area.height - draw_height),
        draw_width,
        draw_height,
    );
    FittedImage::new(source, intrinsic_width, intrinsic_height, destination)
}

/// Computes the destination rectangle for a background image with no intrinsic
/// size or ratio, such as a CSS linear gradient.
///
/// Per CSS, `cover`, `contain` and `auto` all resolve a sizeless image to the
/// full positioning area. Explicit sizes are resolved against the area, with an
/// `auto` component also falling back to the full area dimension (a gradient has
/// no intrinsic ratio to derive the missing component from). `position` still
/// applies to any leftover free space.
pub fn gradient_destination(
    area: Rect,
    size: BackgroundSize,
    position: BackgroundPosition,
) -> Rect {
    if area.is_empty() {
        return Rect::new(area.x, area.y, 0.0, 0.0);
    }

    let (draw_width, draw_height) = match size {
        BackgroundSize::Cover | BackgroundSize::Contain => (area.width, area.height),
        BackgroundSize::Explicit { width, height } => {
            let draw_width = width.map_or(area.width, |value| resolve_size(value, area.width));
            let draw_height = height.map_or(area.height, |value| resolve_size(value, area.height));
            (draw_width, draw_height)
        }
    };

    Rect::new(
        area.x + position.x.resolve(area.width - draw_width),
        area.y + position.y.resolve(area.height - draw_height),
        draw_width,
        draw_height,
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

    #[test]
    fn background_cover_crops_and_centers() {
        let fit = fit_background(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            BackgroundSize::Cover,
            BackgroundPosition::default(),
        );
        assert_eq!(fit.destination, Rect::new(-50.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn background_contain_fits_and_centers() {
        let fit = fit_background(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            BackgroundSize::Contain,
            BackgroundPosition::default(),
        );
        assert_eq!(fit.destination, Rect::new(0.0, 25.0, 100.0, 50.0));
    }

    #[test]
    fn background_size_percentages() {
        let fit = fit_background(
            Rect::new(0.0, 0.0, 200.0, 100.0),
            100,
            100,
            BackgroundSize::Explicit {
                width: Some(LengthOrPercentage::Percentage(0.5)),
                height: Some(LengthOrPercentage::Percentage(1.0)),
            },
            BackgroundPosition::default(),
        );
        // 100x100 drawn in a 200x100 area -> 100px free horizontally.
        assert_eq!(fit.destination, Rect::new(50.0, 0.0, 100.0, 100.0));
    }

    #[test]
    fn background_size_auto_component_preserves_ratio() {
        let fit = fit_background(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            200,
            100,
            BackgroundSize::Explicit {
                width: Some(LengthOrPercentage::Length(50.0)),
                height: None,
            },
            BackgroundPosition::default(),
        );
        // Width 50, height derived from 200x100 ratio -> 25, centered.
        assert_eq!(fit.destination, Rect::new(25.0, 37.5, 50.0, 25.0));
    }

    #[test]
    fn background_default_size_uses_intrinsic_dimensions() {
        let fit = fit_background(
            Rect::new(0.0, 0.0, 200.0, 200.0),
            50,
            40,
            BackgroundSize::default(),
            BackgroundPosition::default(),
        );
        assert_eq!(fit.destination, Rect::new(75.0, 80.0, 50.0, 40.0));
    }

    #[test]
    fn gradient_cover_and_contain_fill_the_area() {
        let area = Rect::new(0.0, 0.0, 100.0, 50.0);
        assert_eq!(
            gradient_destination(area, BackgroundSize::Cover, BackgroundPosition::default()),
            area
        );
        assert_eq!(
            gradient_destination(area, BackgroundSize::Contain, BackgroundPosition::default()),
            area
        );
    }

    #[test]
    fn gradient_explicit_size_is_positioned() {
        let destination = gradient_destination(
            Rect::new(0.0, 0.0, 100.0, 100.0),
            BackgroundSize::Explicit {
                width: Some(LengthOrPercentage::Percentage(0.5)),
                height: Some(LengthOrPercentage::Percentage(0.5)),
            },
            BackgroundPosition::from_percentages(1.0, 1.0),
        );
        assert_eq!(destination, Rect::new(50.0, 50.0, 50.0, 50.0));
    }
}

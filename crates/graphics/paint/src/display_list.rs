//! Display list: the paint primitive stream produced from the layout tree.

use std::hash::{Hash, Hasher};

use browser_css::Color;
use browser_layout::box_model::{Edges, Rect};
use browser_style::BorderStyle;

use crate::images::ImageData;

/// Maximum number of painting commands retained in a display list.
///
/// An adversarial or pathologically large page can otherwise produce millions
/// of commands, all of which must be stored and then walked by the rasterizer.
/// Once the cap is reached, further commands are silently dropped (truncated);
/// already-recorded content is kept so the page still renders deterministically.
pub const MAX_COMMANDS: usize = 500_000;

/// One painting operation.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    /// Pushes a clip rectangle (in device pixels).
    PushClip(Rect),
    /// Pops the most recent clip.
    PopClip,
    /// Fills a rectangle.
    FillRect {
        /// Rectangle in device pixels.
        rect: Rect,
        /// Fill colour.
        color: Color,
    },
    /// Paints a box border.
    DrawBorder {
        /// Border box.
        rect: Rect,
        /// Per-side widths (top, right, bottom, left).
        widths: Edges,
        /// Per-side colours.
        colors: [Color; 4],
        /// Per-side styles.
        styles: [BorderStyle; 4],
    },
    /// Draws a text run.
    DrawText {
        /// Left edge of the first glyph.
        x: f32,
        /// Baseline position.
        baseline: f32,
        /// Text to draw.
        text: String,
        /// Font size in pixels.
        font_size: f32,
        /// Text colour.
        color: Color,
        /// Underline the text.
        underline: bool,
        /// Strike through the text.
        line_through: bool,
        /// Draw with synthetic bold.
        bold: bool,
        /// Draw with synthetic italic.
        italic: bool,
    },
    /// Draws an image scaled into a rectangle.
    DrawImage {
        /// Destination rectangle.
        rect: Rect,
        /// Index into [`DisplayList::images`].
        image: usize,
    },
    /// Fills an anti-aliased rectangle with independently rounded corners.
    DrawRoundedRect {
        /// Rectangle in device-independent pixels.
        rect: Rect,
        /// Corner radii in the order top-left, top-right, bottom-right, bottom-left.
        radius: [f32; 4],
        /// Interior fill.
        background: Color,
        /// Optional border ring, given as `(colour, width)`.
        border: Option<(Color, f32)>,
    },
    /// Draws a soft, offset shadow approximated with concentric rounded rectangles.
    DrawShadow {
        /// Shadow caster rectangle (before the offset is applied).
        rect: Rect,
        /// Corner radii in the order top-left, top-right, bottom-right, bottom-left.
        radius: [f32; 4],
        /// Shadow displacement in device-independent pixels.
        offset: (f32, f32),
        /// Blur spread in device-independent pixels.
        blur: f32,
        /// Shadow colour (its alpha scales the whole shadow).
        color: Color,
    },
    /// Fills an anti-aliased rounded rectangle with a linear colour gradient.
    DrawLinearGradient {
        /// Rectangle in device-independent pixels.
        rect: Rect,
        /// Gradient stops as `(position in 0.0..=1.0, colour)` pairs.
        stops: Vec<(f32, Color)>,
        /// Gradient angle in degrees (0 = upwards, 90 = rightwards).
        angle_deg: f32,
        /// Corner radii in the order top-left, top-right, bottom-right, bottom-left.
        radius: [f32; 4],
    },
    /// Rasterizes a group of commands through an affine transform and opacity.
    ///
    /// Children are painted into a temporary transparent buffer first; the
    /// rasterizer then inverse-maps destination samples into child space so
    /// translation, scale, rotation and skewing are all supported uniformly.
    DrawTransformed {
        /// Commands to paint in the new coordinate system.
        child_commands: Vec<DisplayCommand>,
        /// Affine matrix `[a, b, c, d, e, f]` mapping child CSS pixels to parent
        /// CSS pixels: `x' = a*x + c*y + e`, `y' = b*x + d*y + f`.
        matrix: [f32; 6],
        /// Overall opacity applied to the whole group (0 = invisible, 1 = opaque).
        opacity: f32,
    },
    /// Applies a chain of CSS filters to a group of commands.
    DrawFiltered {
        /// Commands to paint into the temporary filter buffer.
        child_commands: Vec<DisplayCommand>,
        /// Filters applied in order.
        filters: Vec<DisplayFilter>,
    },
}

/// A single CSS filter operation stored in a [`DisplayCommand::DrawFiltered`].
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayFilter {
    /// Gaussian-like blur radius in CSS pixels (approximated with box blurs).
    Blur(f32),
    /// Multiplies RGB by a factor.
    Brightness(f32),
    /// Shifts RGB contrast around mid-grey.
    Contrast(f32),
    /// Desaturates towards luminance (1 = fully grey).
    Grayscale(f32),
    /// Rotates hue by an angle in degrees.
    HueRotate(f32),
    /// Inverts RGB (1 = fully inverted).
    Invert(f32),
    /// Scales alpha (1 = unchanged).
    Opacity(f32),
    /// Scales colour saturation (1 = unchanged, 0 = grey).
    Saturate(f32),
    /// Applies the sepia tone (1 = fully sepia).
    Sepia(f32),
}

/// Small, deterministic FNV-1a hasher used for render-cache keys.
///
/// `DefaultHasher` would also work, but its exact algorithm is not part of the
/// language contract; owning the hasher keeps cache keys stable across builds.
struct FnvHasher(u64);

impl FnvHasher {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u64::from(byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }
}

/// Hashes an `f32` by its bit pattern so `-0.0`, `NaN` and infinities are
/// distinguished and the result is reproducible.
fn hash_f32<H: Hasher>(state: &mut H, value: f32) {
    state.write_u64(u64::from(value.to_bits()));
}

fn hash_rect<H: Hasher>(state: &mut H, rect: Rect) {
    hash_f32(state, rect.x);
    hash_f32(state, rect.y);
    hash_f32(state, rect.width);
    hash_f32(state, rect.height);
}

fn hash_edges<H: Hasher>(state: &mut H, edges: Edges) {
    hash_f32(state, edges.top);
    hash_f32(state, edges.right);
    hash_f32(state, edges.bottom);
    hash_f32(state, edges.left);
}

fn hash_border_style<H: Hasher>(state: &mut H, style: BorderStyle) {
    let tag = match style {
        BorderStyle::None => 0u8,
        BorderStyle::Hidden => 1,
        BorderStyle::Solid => 2,
        BorderStyle::Dashed => 3,
        BorderStyle::Dotted => 4,
        BorderStyle::Double => 5,
        BorderStyle::Groove => 6,
        BorderStyle::Ridge => 7,
        BorderStyle::Inset => 8,
        BorderStyle::Outset => 9,
    };
    state.write_u8(tag);
}

impl DisplayCommand {
    /// Feeds one command into `state` so structurally identical streams hash
    /// identically. Used by [`DisplayList::identity_hash`].
    fn hash_into<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::PushClip(rect) => {
                state.write_u8(0);
                hash_rect(state, *rect);
            }
            Self::PopClip => state.write_u8(1),
            Self::FillRect { rect, color } => {
                state.write_u8(2);
                hash_rect(state, *rect);
                color.hash(state);
            }
            Self::DrawBorder {
                rect,
                widths,
                colors,
                styles,
            } => {
                state.write_u8(3);
                hash_rect(state, *rect);
                hash_edges(state, *widths);
                for color in colors {
                    color.hash(state);
                }
                for style in styles {
                    hash_border_style(state, *style);
                }
            }
            Self::DrawText {
                x,
                baseline,
                text,
                font_size,
                color,
                underline,
                line_through,
                bold,
                italic,
            } => {
                state.write_u8(4);
                hash_f32(state, *x);
                hash_f32(state, *baseline);
                state.write_usize(text.len());
                state.write(text.as_bytes());
                hash_f32(state, *font_size);
                color.hash(state);
                for flag in [*underline, *line_through, *bold, *italic] {
                    state.write_u8(u8::from(flag));
                }
            }
            Self::DrawImage { rect, image } => {
                state.write_u8(5);
                hash_rect(state, *rect);
                state.write_usize(*image);
            }
            Self::DrawRoundedRect {
                rect,
                radius,
                background,
                border,
            } => {
                state.write_u8(6);
                hash_rect(state, *rect);
                for value in radius {
                    hash_f32(state, *value);
                }
                background.hash(state);
                match border {
                    Some((color, width)) => {
                        state.write_u8(1);
                        color.hash(state);
                        hash_f32(state, *width);
                    }
                    None => state.write_u8(0),
                }
            }
            Self::DrawShadow {
                rect,
                radius,
                offset,
                blur,
                color,
            } => {
                state.write_u8(7);
                hash_rect(state, *rect);
                for value in radius {
                    hash_f32(state, *value);
                }
                hash_f32(state, offset.0);
                hash_f32(state, offset.1);
                hash_f32(state, *blur);
                color.hash(state);
            }
            Self::DrawLinearGradient {
                rect,
                stops,
                angle_deg,
                radius,
            } => {
                state.write_u8(8);
                hash_rect(state, *rect);
                state.write_usize(stops.len());
                for (position, color) in stops {
                    hash_f32(state, *position);
                    color.hash(state);
                }
                hash_f32(state, *angle_deg);
                for value in radius {
                    hash_f32(state, *value);
                }
            }
            Self::DrawTransformed {
                child_commands,
                matrix,
                opacity,
            } => {
                state.write_u8(9);
                state.write_usize(child_commands.len());
                for command in child_commands {
                    command.hash_into(state);
                }
                for value in matrix {
                    hash_f32(state, *value);
                }
                hash_f32(state, *opacity);
            }
            Self::DrawFiltered {
                child_commands,
                filters,
            } => {
                state.write_u8(10);
                state.write_usize(child_commands.len());
                for command in child_commands {
                    command.hash_into(state);
                }
                state.write_usize(filters.len());
                for filter in filters {
                    let (tag, value) = match *filter {
                        DisplayFilter::Blur(v) => (0u8, v),
                        DisplayFilter::Brightness(v) => (1, v),
                        DisplayFilter::Contrast(v) => (2, v),
                        DisplayFilter::Grayscale(v) => (3, v),
                        DisplayFilter::HueRotate(v) => (4, v),
                        DisplayFilter::Invert(v) => (5, v),
                        DisplayFilter::Opacity(v) => (6, v),
                        DisplayFilter::Saturate(v) => (7, v),
                        DisplayFilter::Sepia(v) => (8, v),
                    };
                    state.write_u8(tag);
                    hash_f32(state, value);
                }
            }
        }
    }
}

/// A list of painting operations plus the images they reference.
#[derive(Debug, Clone, Default)]
pub struct DisplayList {
    /// Commands in painting order.
    pub commands: Vec<DisplayCommand>,
    /// Images referenced by `DrawImage`.
    pub images: Vec<ImageData>,
}

impl DisplayList {
    /// Creates an empty display list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a command, unless the list is already at [`MAX_COMMANDS`].
    pub fn push(&mut self, command: DisplayCommand) {
        if self.commands.len() < MAX_COMMANDS {
            self.commands.push(command);
        }
    }

    /// Appends a command and returns its index.
    ///
    /// Returns `usize::MAX` when the list is at [`MAX_COMMANDS`] and the command
    /// was dropped; callers must treat that value as "not recorded".
    pub fn push_indexed(&mut self, command: DisplayCommand) -> usize {
        if self.commands.len() >= MAX_COMMANDS {
            return usize::MAX;
        }
        let index = self.commands.len();
        self.commands.push(command);
        index
    }

    /// True when the list has stopped accepting commands because it reached
    /// [`MAX_COMMANDS`].
    pub fn is_truncated(&self) -> bool {
        self.commands.len() >= MAX_COMMANDS
    }

    /// Number of commands.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// True when nothing is painted.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// Registers an image, returning its index.
    pub fn add_image(&mut self, image: ImageData) -> usize {
        self.images.push(image);
        self.images.len() - 1
    }

    /// Deterministic structural hash of the command stream and images.
    ///
    /// Two lists whose painting instructions (and image contents) are identical
    /// produce the same value, even when they were built into different `Vec`s.
    /// This lets the render cache recognise an unchanged page across frames.
    pub fn identity_hash(&self) -> u64 {
        let mut hasher = FnvHasher::new();
        hasher.write_usize(self.commands.len());
        for command in &self.commands {
            command.hash_into(&mut hasher);
        }
        hasher.write_usize(self.images.len());
        for image in &self.images {
            hasher.write_u32(image.width);
            hasher.write_u32(image.height);
            hasher.write(&image.pixels);
        }
        hasher.finish()
    }

    /// Number of fill commands (used by tests and the profiler).
    pub fn fill_count(&self) -> usize {
        self.commands
            .iter()
            .filter(|c| matches!(c, DisplayCommand::FillRect { .. }))
            .count()
    }

    /// Number of clip push commands.
    pub fn push_clip_count(&self) -> usize {
        self.commands
            .iter()
            .filter(|c| matches!(c, DisplayCommand::PushClip(_)))
            .count()
    }

    /// Number of text commands.
    pub fn text_count(&self) -> usize {
        self.commands
            .iter()
            .filter(|c| matches!(c, DisplayCommand::DrawText { .. }))
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_commands() {
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            color: Color::rgb(255, 0, 0),
        });
        list.push(DisplayCommand::DrawText {
            x: 0.0,
            baseline: 10.0,
            text: "hi".into(),
            font_size: 16.0,
            color: Color::rgb(0, 0, 0),
            underline: false,
            line_through: false,
            bold: false,
            italic: false,
        });
        assert_eq!(list.len(), 2);
        assert_eq!(list.fill_count(), 1);
        assert_eq!(list.text_count(), 1);
    }

    #[test]
    fn identity_hash_tracks_content_not_allocation() {
        let build = |color| {
            let mut list = DisplayList::new();
            list.push(DisplayCommand::FillRect {
                rect: Rect::new(1.0, 2.0, 3.0, 4.0),
                color,
            });
            list.push(DisplayCommand::DrawText {
                x: 0.0,
                baseline: 10.0,
                text: "hi".into(),
                font_size: 16.0,
                color: Color::rgb(0, 0, 0),
                underline: false,
                line_through: false,
                bold: false,
                italic: false,
            });
            list
        };
        let red = Color::rgb(255, 0, 0);
        let blue = Color::rgb(0, 0, 255);
        assert_eq!(build(red).identity_hash(), build(red).identity_hash());
        assert_ne!(build(red).identity_hash(), build(blue).identity_hash());
    }

    #[test]
    fn command_count_is_capped() {
        let mut list = DisplayList::new();
        // Fill right up to the cap with a cheap command.
        list.commands.resize(MAX_COMMANDS, DisplayCommand::PopClip);
        assert!(list.is_truncated());

        list.push(DisplayCommand::FillRect {
            rect: Rect::new(0.0, 0.0, 1.0, 1.0),
            color: Color::rgb(1, 2, 3),
        });
        let index = list.push_indexed(DisplayCommand::PopClip);
        assert_eq!(
            list.len(),
            MAX_COMMANDS,
            "commands past the cap are dropped"
        );
        assert_eq!(
            index,
            usize::MAX,
            "dropped indexed command reports no index"
        );
    }
}

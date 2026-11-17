//! Display list: the paint primitive stream produced from the layout tree.

use browser_css::Color;
use browser_layout::box_model::{Edges, Rect};
use browser_style::BorderStyle;

use crate::images::ImageData;

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

    /// Appends a command.
    pub fn push(&mut self, command: DisplayCommand) {
        self.commands.push(command);
    }

    /// Appends a command and returns its index.
    pub fn push_indexed(&mut self, command: DisplayCommand) -> usize {
        let index = self.commands.len();
        self.commands.push(command);
        index
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
}

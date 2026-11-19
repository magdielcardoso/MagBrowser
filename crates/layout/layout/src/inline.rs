//! Inline formatting: line boxes, text fragments and wrapping.

use browser_dom::NodeId;
use browser_style::{ComputedStyle, Overflow, TextAlign, WhiteSpace};

use crate::box_model::Rect;

/// A run of text positioned on a line.
#[derive(Debug, Clone, PartialEq)]
pub struct TextFragment {
    /// The text node this fragment belongs to.
    pub node: NodeId,
    /// Text content of the fragment.
    pub text: String,
    /// Left edge in document coordinates.
    pub x: f32,
    /// Baseline position in document coordinates.
    pub baseline: f32,
    /// Width in CSS pixels.
    pub width: f32,
    /// Font size in pixels.
    pub font_size: f32,
    /// Text colour.
    pub color: browser_style::Color,
    /// True when the text should be underlined.
    pub underline: bool,
    /// Line-through decoration.
    pub line_through: bool,
    /// Synthetic bold.
    pub bold: bool,
    /// Synthetic italic.
    pub italic: bool,
}

/// A laid-out line of inline content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineBox {
    /// Bounding rectangle of the line.
    pub rect: Rect,
    /// Baseline offset from the line top.
    pub baseline: f32,
    /// Fragments in visual order.
    pub fragments: Vec<TextFragment>,
    /// Atomic inline-level boxes (inline-blocks, images) on this line.
    pub atoms: Vec<LayoutAtom>,
}

/// An atomic inline box placed on a line.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutAtom {
    /// Box index in the layout tree.
    pub box_index: usize,
    /// Rectangle of the atom.
    pub rect: Rect,
}

/// One measurable inline item.
#[derive(Debug, Clone)]
pub(crate) enum InlineItem {
    Text {
        node: NodeId,
        text: String,
        width: f32,
        font_size: f32,
        color: browser_style::Color,
        underline: bool,
        line_through: bool,
        bold: bool,
        italic: bool,
        break_before: bool,
    },
    Space {
        width: f32,
        font_size: f32,
    },
    ForcedBreak {
        font_size: f32,
    },
    Atom {
        box_index: usize,
        width: f32,
        height: f32,
        baseline: f32,
    },
}

/// Computes the effective white-space behaviour.
pub(crate) fn whitespace_mode(style: &ComputedStyle) -> WhiteSpace {
    style.white_space()
}

/// Collapses whitespace in a text run according to `white-space`.
pub(crate) fn collapse_whitespace(text: &str, mode: WhiteSpace) -> String {
    match mode {
        WhiteSpace::Normal | WhiteSpace::Nowrap => {
            let mut out = String::with_capacity(text.len());
            let mut last_space = false;
            for ch in text.chars() {
                if ch.is_whitespace() {
                    if !last_space {
                        out.push(' ');
                        last_space = true;
                    }
                } else {
                    out.push(ch);
                    last_space = false;
                }
            }
            out
        }
        WhiteSpace::Pre | WhiteSpace::PreWrap => text.to_string(),
        WhiteSpace::PreLine => {
            let mut out = String::with_capacity(text.len());
            let mut last_space = false;
            for ch in text.chars() {
                if ch == '\n' {
                    out.push('\n');
                    last_space = false;
                } else if ch.is_whitespace() {
                    if !last_space {
                        out.push(' ');
                        last_space = true;
                    }
                } else {
                    out.push(ch);
                    last_space = false;
                }
            }
            out
        }
    }
}

/// A float band that shortens the lines it overlaps.
///
/// X coordinates are relative to the line container's content box; Y
/// coordinates are absolute document coordinates, matching line boxes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FloatBand {
    /// True for a left float, false for a right float.
    pub left: bool,
    /// Float border box.
    pub rect: Rect,
}

/// Greedy line breaker over inline items.
pub(crate) struct LineBreaker {
    width: f32,
    next_y: f32,
    lines: Vec<LineBox>,
    line_spans: Vec<(f32, f32)>,
    current: Vec<PendingFragment>,
    current_width: f32,
    current_height: f32,
    current_baseline: f32,
    atoms: Vec<LayoutAtom>,
    floats: Vec<FloatBand>,
    span_left: f32,
    span_width: f32,
}

struct PendingFragment {
    node: NodeId,
    text: String,
    x: f32,
    width: f32,
    font_size: f32,
    color: browser_style::Color,
    underline: bool,
    line_through: bool,
    bold: bool,
    italic: bool,
}

impl LineBreaker {
    pub fn new(width: f32, start_y: f32) -> Self {
        let width = width.max(1.0);
        Self {
            width,
            next_y: start_y,
            lines: Vec::new(),
            line_spans: Vec::new(),
            current: Vec::new(),
            current_width: 0.0,
            current_height: 0.0,
            current_baseline: 0.0,
            atoms: Vec::new(),
            floats: Vec::new(),
            span_left: 0.0,
            span_width: width,
        }
    }

    /// Installs the floats that shorten the lines of this container.
    pub fn set_floats(&mut self, bands: Vec<FloatBand>) {
        self.floats = bands;
        if self.current.is_empty() && self.current_width == 0.0 {
            self.begin_line();
        }
    }

    /// The horizontal span available for a band at `y`.
    fn span_at(&self, y: f32, height: f32) -> (f32, f32) {
        let mut left = 0.0_f32;
        let mut right = self.width;
        for band in &self.floats {
            if band.rect.bottom() <= y || band.rect.y >= y + height {
                continue;
            }
            if band.left {
                left = left.max(band.rect.right());
            } else {
                right = right.min(band.rect.x);
            }
        }
        let left = left.min(right);
        (left, (right - left).max(0.0))
    }

    /// Chooses the span for the next line, dropping below floats that block it
    /// completely.
    fn begin_line(&mut self) {
        let nominal_height = 16.0_f32;
        let mut y = self.next_y;
        let mut span = self.span_at(y, nominal_height);
        let mut guard = 0_usize;
        while span.1 <= 0.01 && guard <= self.floats.len() {
            let next = self
                .floats
                .iter()
                .filter(|band| band.rect.bottom() > y && band.rect.y < y + nominal_height)
                .map(|band| band.rect.bottom())
                .fold(f32::INFINITY, f32::min);
            if !next.is_finite() {
                break;
            }
            y = next;
            span = self.span_at(y, nominal_height);
            guard += 1;
        }
        self.next_y = y;
        self.span_left = span.0;
        self.span_width = span.1.max(1.0);
    }

    fn line_height(font_size: f32) -> f32 {
        (font_size * 1.2).max(font_size)
    }

    pub fn push(&mut self, item: InlineItem) {
        match item {
            InlineItem::Text {
                node,
                text,
                width,
                font_size,
                color,
                underline,
                line_through,
                bold,
                italic,
                break_before,
            } => {
                if break_before
                    && self.current_width > 0.0
                    && self.current_width + width > self.span_width
                {
                    self.flush();
                }
                self.current.push(PendingFragment {
                    node,
                    text,
                    x: self.current_width,
                    width,
                    font_size,
                    color,
                    underline,
                    line_through,
                    bold,
                    italic,
                });
                self.current_width += width;
                self.current_height = self.current_height.max(Self::line_height(font_size));
                self.current_baseline = self.current_baseline.max(font_size * 0.8);
            }
            InlineItem::Space { width, font_size } => {
                if self.current.is_empty() {
                    return;
                }
                self.current_width += width;
                self.current_height = self.current_height.max(Self::line_height(font_size));
            }
            InlineItem::ForcedBreak { font_size } => {
                self.current_height = self.current_height.max(Self::line_height(font_size));
                self.flush();
            }
            InlineItem::Atom {
                box_index,
                width,
                height,
                baseline,
            } => {
                if self.current_width > 0.0 && self.current_width + width > self.span_width {
                    self.flush();
                }
                self.atoms.push(LayoutAtom {
                    box_index,
                    rect: Rect::new(self.current_width, 0.0, width, height),
                });
                self.current_width += width;
                self.current_height = self.current_height.max(height);
                self.current_baseline = self.current_baseline.max(baseline);
            }
        }
    }

    fn flush(&mut self) {
        let line_y = self.next_y;
        let height = self.current_height.max(16.0);
        let baseline = self.current_baseline.max(height * 0.8);
        let rect = Rect::new(self.span_left, line_y, self.current_width, height);
        let fragments = self
            .current
            .drain(..)
            .map(|p| TextFragment {
                node: p.node,
                text: p.text,
                x: self.span_left + p.x,
                baseline: line_y + baseline,
                width: p.width,
                font_size: p.font_size,
                color: p.color,
                underline: p.underline,
                line_through: p.line_through,
                bold: p.bold,
                italic: p.italic,
            })
            .collect();
        let mut line = LineBox {
            rect,
            baseline,
            fragments,
            atoms: std::mem::take(&mut self.atoms),
        };
        line.atoms = line
            .atoms
            .into_iter()
            .map(|mut atom| {
                atom.rect.x += self.span_left;
                atom.rect.y = line_y + (height - atom.rect.height).max(0.0);
                atom
            })
            .collect();
        self.lines.push(line);
        self.line_spans.push((self.span_left, self.span_width));
        self.next_y += height;
        self.current_width = 0.0;
        self.current_height = 0.0;
        self.current_baseline = 0.0;
        self.begin_line();
    }

    pub fn finish(mut self, align: TextAlign, container_width: f32) -> Vec<LineBox> {
        if !self.current.is_empty() || !self.atoms.is_empty() {
            self.flush();
        }
        let mut lines = self.lines;
        let spans = std::mem::take(&mut self.line_spans);
        for (line, (left, width)) in lines.iter_mut().zip(spans) {
            // `width` is the available inline span after floats. Fall back to
            // the containing block width if no span was recorded for the line.
            let available = if width > 0.0 {
                width
            } else {
                container_width.max(0.0)
            };
            let offset = match align {
                TextAlign::Left | TextAlign::Justify => 0.0,
                TextAlign::Right => (available - line.rect.width).max(0.0),
                TextAlign::Center => ((available - line.rect.width) / 2.0).max(0.0),
            };
            if offset > 0.0 {
                // Shift the line rectangle, every text fragment **and** every
                // atomic inline box together. Centring only the text would
                // leave images and inline-blocks pinned to the left edge.
                line.rect.x = left + offset;
                for fragment in &mut line.fragments {
                    fragment.x += offset;
                }
                for atom in &mut line.atoms {
                    atom.rect.x += offset;
                }
            }
        }
        lines
    }
}

/// True when a style establishes a scroll container for the given axis.
pub(crate) fn is_scroll_container(style: &ComputedStyle) -> bool {
    !matches!(style.overflow_y(), Overflow::Visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::NodeId;

    #[test]
    fn collapses_whitespace_in_normal_mode() {
        assert_eq!(
            collapse_whitespace("  hello \n\t world  ", WhiteSpace::Normal),
            " hello world "
        );
    }

    #[test]
    fn preserves_whitespace_in_pre_mode() {
        assert_eq!(collapse_whitespace("  a\n b", WhiteSpace::Pre), "  a\n b");
    }

    #[test]
    fn breaker_wraps_on_width() {
        let mut breaker = LineBreaker::new(10.0, 0.0);
        for i in 0..4 {
            breaker.push(InlineItem::Text {
                node: NodeId(1),
                text: format!("w{i}"),
                width: 5.0,
                font_size: 10.0,
                color: browser_style::Color::rgb(0, 0, 0),
                underline: false,
                line_through: false,
                bold: false,
                italic: false,
                break_before: i > 0,
            });
        }
        let lines = breaker.finish(TextAlign::Left, 10.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].fragments.len(), 2);
    }

    #[test]
    fn center_alignment_offsets_lines() {
        let mut breaker = LineBreaker::new(100.0, 0.0);
        breaker.push(InlineItem::Text {
            node: NodeId(1),
            text: "hi".into(),
            width: 20.0,
            font_size: 10.0,
            color: browser_style::Color::rgb(0, 0, 0),
            underline: false,
            line_through: false,
            bold: false,
            italic: false,
            break_before: false,
        });
        let lines = breaker.finish(TextAlign::Center, 100.0);
        assert_eq!(lines[0].rect.x, 40.0);
        assert_eq!(lines[0].fragments[0].x, 40.0);
    }

    #[test]
    fn center_alignment_offsets_atoms() {
        // A 272px-wide atomic inline box inside an 800px container must be
        // centered just like a run of text: (800 - 272) / 2 == 264.
        let mut breaker = LineBreaker::new(800.0, 0.0);
        breaker.push(InlineItem::Atom {
            box_index: 0,
            width: 272.0,
            height: 92.0,
            baseline: 92.0,
        });
        let lines = breaker.finish(TextAlign::Center, 800.0);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].rect.x, 264.0);
        assert_eq!(lines[0].atoms.len(), 1);
        assert_eq!(lines[0].atoms[0].rect.x, 264.0);
    }

    #[test]
    fn center_alignment_offsets_text_and_atom_together() {
        let mut breaker = LineBreaker::new(800.0, 0.0);
        breaker.push(InlineItem::Text {
            node: NodeId(1),
            text: "hi".into(),
            width: 100.0,
            font_size: 10.0,
            color: browser_style::Color::rgb(0, 0, 0),
            underline: false,
            line_through: false,
            bold: false,
            italic: false,
            break_before: false,
        });
        breaker.push(InlineItem::Atom {
            box_index: 0,
            width: 272.0,
            height: 92.0,
            baseline: 92.0,
        });
        let lines = breaker.finish(TextAlign::Center, 800.0);
        assert_eq!(lines.len(), 1);
        let offset = (800.0 - 372.0) / 2.0;
        assert_eq!(lines[0].fragments[0].x, offset);
        assert_eq!(lines[0].atoms[0].rect.x, offset + 100.0);
    }

    #[test]
    fn img_atom_is_centered_by_inherited_text_align() {
        let mut doc = browser_dom::Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let center = doc.create_element("center");
        doc.append_child(body, center);
        let div = doc.create_element("div");
        doc.append_child(center, div);
        let img = doc.create_element("img");
        doc.set_attr(img, "width", "272");
        doc.set_attr(img, "height", "92");
        doc.append_child(div, img);

        let engine = browser_style::StyleEngine::new(browser_style::Viewport {
            width: 800.0,
            height: 600.0,
        });
        let styled = engine.compute(&doc);
        let tree = crate::layout(
            &doc,
            &styled,
            crate::Size {
                width: 800.0,
                height: 600.0,
            },
        );
        let img_node = tree.find_node(img).unwrap().node.unwrap();
        let img_box = tree
            .boxes
            .iter()
            .find(|b| b.node == Some(img_node))
            .unwrap();
        assert!(
            (img_box.rect.x - 264.0).abs() < 1.0,
            "image x = {}",
            img_box.rect.x
        );
    }
}

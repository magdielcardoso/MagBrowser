//! Clip stack used while walking the paint tree.

use browser_layout::Rect;

/// A stack of intersecting clip rectangles.
#[derive(Debug, Clone, Default)]
pub struct ClipStack {
    rects: Vec<Rect>,
}

impl ClipStack {
    /// Creates an empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pushes a clip, intersected with the current clip.
    pub fn push(&mut self, rect: Rect) {
        let rect = match self.current() {
            Some(current) => current.intersect(&rect),
            None => rect,
        };
        self.rects.push(rect);
    }

    /// Pops the most recent clip.
    pub fn pop(&mut self) -> Option<Rect> {
        self.rects.pop()
    }

    /// The effective clip rectangle, if any.
    pub fn current(&self) -> Option<Rect> {
        self.rects.last().copied()
    }

    /// True when a point survives every clip.
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.current().is_none_or(|clip| clip.contains(x, y))
    }

    /// Number of stacked clips.
    pub fn depth(&self) -> usize {
        self.rects.len()
    }

    /// Clears the stack.
    pub fn clear(&mut self) {
        self.rects.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_intersect() {
        let mut stack = ClipStack::new();
        stack.push(Rect::new(0.0, 0.0, 100.0, 100.0));
        stack.push(Rect::new(50.0, 50.0, 100.0, 100.0));
        assert_eq!(stack.current(), Some(Rect::new(50.0, 50.0, 50.0, 50.0)));
        assert!(stack.contains(60.0, 60.0));
        assert!(!stack.contains(10.0, 10.0));
        assert_eq!(stack.depth(), 2);
        stack.pop();
        assert_eq!(stack.current(), Some(Rect::new(0.0, 0.0, 100.0, 100.0)));
    }

    #[test]
    fn empty_stack_contains_everything() {
        let stack = ClipStack::new();
        assert!(stack.contains(-1000.0, 9999.0));
        assert_eq!(stack.current(), None);
    }
}

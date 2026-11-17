//! Damage tracking for incremental repaints.

use browser_layout::{Rect, Size};

/// A set of rectangles that need to be repainted.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Damage {
    rects: Vec<Rect>,
}

impl Damage {
    /// Creates an empty damage set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Damage covering the whole viewport.
    pub fn full(viewport: Size) -> Self {
        Self {
            rects: vec![Rect::new(0.0, 0.0, viewport.width, viewport.height)],
        }
    }

    /// Adds a damaged rectangle, merging it with overlapping rectangles.
    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        let mut merged = rect;
        let mut index = 0;
        while index < self.rects.len() {
            let other = self.rects[index];
            if overlaps(&merged, &other) {
                merged = merged.union(&other);
                self.rects.remove(index);
            } else {
                index += 1;
            }
        }
        self.rects.push(merged);
    }

    /// Adds every rectangle from another damage set.
    pub fn add_all(&mut self, other: &Damage) {
        for rect in &other.rects {
            self.add(*rect);
        }
    }

    /// True when nothing needs repainting.
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// The damaged rectangles.
    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Total damaged area in square pixels.
    pub fn area(&self) -> f32 {
        self.rects.iter().map(|r| r.width * r.height).sum()
    }

    /// Removes all damage.
    pub fn clear(&mut self) {
        self.rects.clear();
    }
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x < b.right() && b.x < a.right() && a.y < b.bottom() && b.y < a.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_overlapping_rects() {
        let mut damage = Damage::new();
        damage.add(Rect::new(0.0, 0.0, 10.0, 10.0));
        damage.add(Rect::new(5.0, 5.0, 10.0, 10.0));
        assert_eq!(damage.rects().len(), 1);
        assert_eq!(damage.area(), 225.0);
    }

    #[test]
    fn keeps_disjoint_rects() {
        let mut damage = Damage::new();
        damage.add(Rect::new(0.0, 0.0, 10.0, 10.0));
        damage.add(Rect::new(50.0, 50.0, 10.0, 10.0));
        assert_eq!(damage.rects().len(), 2);
        assert_eq!(damage.area(), 200.0);
    }

    #[test]
    fn full_damage_covers_viewport() {
        let damage = Damage::full(Size {
            width: 800.0,
            height: 600.0,
        });
        assert_eq!(damage.area(), 480_000.0);
    }
}

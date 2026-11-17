//! Tile grid with dirty-region tracking.

use browser_layout::Rect;

/// A grid of fixed-size tiles covering a layer.
#[derive(Debug, Clone)]
pub struct TileGrid {
    tile_size: u32,
    columns: u32,
    rows: u32,
    dirty: Vec<bool>,
}

impl TileGrid {
    /// Creates a grid covering `width` × `height` device pixels.
    pub fn new(tile_size: u32, width: u32, height: u32) -> Self {
        let tile_size = tile_size.max(1);
        let columns = width.div_ceil(tile_size).max(1);
        let rows = height.div_ceil(tile_size).max(1);
        Self {
            tile_size,
            columns,
            rows,
            dirty: vec![true; (columns * rows) as usize],
        }
    }

    /// Tile size in pixels.
    pub fn tile_size(&self) -> u32 {
        self.tile_size
    }

    /// Number of tile columns.
    pub fn columns(&self) -> u32 {
        self.columns
    }

    /// Number of tile rows.
    pub fn rows(&self) -> u32 {
        self.rows
    }

    /// Total number of tiles.
    pub fn len(&self) -> usize {
        self.dirty.len()
    }

    /// True when the grid has no tiles (never for a valid grid).
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    /// Marks every tile intersecting `rect` as dirty.
    pub fn mark_dirty(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        let (c0, r0) = self.tile_coords(rect.x.max(0.0), rect.y.max(0.0));
        let (c1, r1) = self.tile_coords(
            (rect.right() - 0.001).max(0.0),
            (rect.bottom() - 0.001).max(0.0),
        );
        for row in r0..=r1.min(self.rows.saturating_sub(1)) {
            for col in c0..=c1.min(self.columns.saturating_sub(1)) {
                let index = (row * self.columns + col) as usize;
                if let Some(slot) = self.dirty.get_mut(index) {
                    *slot = true;
                }
            }
        }
    }

    /// Marks every tile dirty.
    pub fn mark_all(&mut self) {
        self.dirty.iter_mut().for_each(|d| *d = true);
    }

    /// True when the tile at (column, row) needs repainting.
    pub fn is_dirty(&self, column: u32, row: u32) -> bool {
        self.dirty
            .get((row * self.columns + column) as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Number of dirty tiles.
    pub fn dirty_count(&self) -> usize {
        self.dirty.iter().filter(|d| **d).count()
    }

    /// Clears all dirty flags, returning the bounding rects of the dirty tiles.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        let mut rects = Vec::new();
        for row in 0..self.rows {
            for col in 0..self.columns {
                let index = (row * self.columns + col) as usize;
                if self.dirty[index] {
                    self.dirty[index] = false;
                    rects.push(Rect::new(
                        (col * self.tile_size) as f32,
                        (row * self.tile_size) as f32,
                        self.tile_size as f32,
                        self.tile_size as f32,
                    ));
                }
            }
        }
        rects
    }

    fn tile_coords(&self, x: f32, y: f32) -> (u32, u32) {
        ((x as u32) / self.tile_size, (y as u32) / self.tile_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_grid_starts_dirty() {
        let grid = TileGrid::new(64, 200, 100);
        assert_eq!(grid.columns(), 4);
        assert_eq!(grid.rows(), 2);
        assert_eq!(grid.dirty_count(), 8);
    }

    #[test]
    fn marks_intersecting_tiles() {
        let mut grid = TileGrid::new(64, 200, 200);
        grid.take_damage();
        assert_eq!(grid.dirty_count(), 0);
        grid.mark_dirty(Rect::new(10.0, 10.0, 10.0, 10.0));
        assert_eq!(grid.dirty_count(), 1);
        assert!(grid.is_dirty(0, 0));
        grid.mark_dirty(Rect::new(60.0, 60.0, 10.0, 10.0));
        assert_eq!(grid.dirty_count(), 4);
    }

    #[test]
    fn take_damage_returns_tile_rects_and_clears() {
        let mut grid = TileGrid::new(32, 64, 32);
        let rects = grid.take_damage();
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0], Rect::new(0.0, 0.0, 32.0, 32.0));
        assert_eq!(grid.dirty_count(), 0);
        assert!(grid.take_damage().is_empty());
    }
}

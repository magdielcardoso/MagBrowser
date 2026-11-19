//! Tile grid with dirty-region tracking.

use browser_layout::Rect;

/// Maximum number of dirty-tracking tiles allocated by a single [`TileGrid`].
///
/// A huge layer (for example a `100000x100000` CSS box) must not turn into a
/// multi-gigabyte `Vec<bool>`. When a grid would exceed this budget the tile
/// size is grown until the grid fits, trading damage precision for a bounded,
/// predictable allocation. 1,048,576 tiles cost roughly 1 MiB.
pub const MAX_TILES: usize = 1024 * 1024;

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
    ///
    /// The effective tile size is increased as needed so the grid never holds
    /// more than [`MAX_TILES`] tiles.
    pub fn new(tile_size: u32, width: u32, height: u32) -> Self {
        let mut tile_size = tile_size.max(1);
        let (columns, rows) = loop {
            let columns = u64::from(width).div_ceil(u64::from(tile_size)).max(1);
            let rows = u64::from(height).div_ceil(u64::from(tile_size)).max(1);
            if columns.saturating_mul(rows) <= MAX_TILES as u64 {
                break (columns as u32, rows as u32);
            }
            // Growing the tile shrinks both axes; at `u32::MAX` any u32 width or
            // height is a single tile, so the loop always terminates.
            tile_size = tile_size.saturating_mul(2);
        };
        Self {
            tile_size,
            columns,
            rows,
            dirty: vec![true; (columns as usize) * (rows as usize)],
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

    #[test]
    fn huge_layer_tile_count_is_capped() {
        let grid = TileGrid::new(1, 100_000, 100_000);
        assert!(grid.len() <= MAX_TILES);
        assert!(grid.tile_size() > 1, "tile size grows to fit the budget");
        assert_eq!(grid.dirty_count(), grid.len());

        // Even u32::MAX dimensions must terminate and stay bounded.
        let extreme = TileGrid::new(1, u32::MAX, u32::MAX);
        assert!(extreme.len() <= MAX_TILES);
    }
}

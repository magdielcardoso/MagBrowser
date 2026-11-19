//! Layer tree built from the layout tree.

use browser_layout::{LayoutTree, Rect, Size};

use crate::damage::Damage;
use crate::tiles::TileGrid;

/// Identifier of a compositor layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerId(pub usize);

/// One composited layer.
#[derive(Debug, Clone)]
pub struct Layer {
    /// Layer id.
    pub id: LayerId,
    /// Layout box that generated the layer.
    pub box_index: usize,
    /// Bounds in document coordinates.
    pub bounds: Rect,
    /// Scroll offset applied to this layer.
    pub scroll: (f32, f32),
    /// Stacking order.
    pub z_index: i32,
    /// Tiles for incremental painting.
    pub tiles: TileGrid,
    /// Opacity applied at composite time.
    pub opacity: f32,
}

impl Layer {
    /// Marks the whole layer dirty.
    pub fn invalidate(&mut self) {
        self.tiles.mark_all();
    }

    /// Marks a region dirty.
    pub fn invalidate_rect(&mut self, rect: Rect) {
        self.tiles
            .mark_dirty(rect.translate(-self.bounds.x, -self.bounds.y));
    }

    /// Damage accumulated on this layer.
    pub fn take_damage(&mut self) -> Damage {
        let mut damage = Damage::new();
        for rect in self.tiles.take_damage() {
            damage.add(rect.translate(self.bounds.x, self.bounds.y));
        }
        damage
    }
}

/// A GPU-agnostic snapshot of one layer at composite time.
///
/// This is the bridge between the layer tree and a backend: the GPU presenter
/// turns the affine `transform` into its shader matrix and uses `opacity` and
/// `z_index` to order and draw the layer. It deliberately holds no pixels and
/// no wgpu types so the compositor stays GPU-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayerComposite {
    /// Layer that produced this snapshot.
    pub id: LayerId,
    /// Stacking order; larger values are drawn later.
    pub z_index: i32,
    /// Opacity in `[0, 1]`.
    pub opacity: f32,
    /// Unscrolled bounds in document coordinates.
    pub bounds: Rect,
    /// Scroll offset applied by this layer.
    pub scroll: (f32, f32),
    /// Affine `[a, b, c, d, e, f]` mapping local pixels to document pixels.
    pub transform: [f32; 6],
}

impl LayerComposite {
    /// Snapshots a layer's current composite state.
    pub fn from_layer(layer: &Layer) -> Self {
        Self {
            id: layer.id,
            z_index: layer.z_index,
            opacity: layer.opacity,
            bounds: layer.bounds,
            scroll: layer.scroll,
            transform: [
                1.0,
                0.0,
                0.0,
                1.0,
                layer.bounds.x - layer.scroll.0,
                layer.bounds.y - layer.scroll.1,
            ],
        }
    }

    /// Maps a local point into document coordinates.
    pub fn map_point(&self, x: f32, y: f32) -> (f32, f32) {
        let t = &self.transform;
        (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5])
    }

    /// Bounds after applying the layer's scroll offset.
    pub fn visible_bounds(&self) -> Rect {
        Rect::new(
            self.bounds.x - self.scroll.0,
            self.bounds.y - self.scroll.1,
            self.bounds.width,
            self.bounds.height,
        )
    }
}

/// The compositor layer tree.
#[derive(Debug, Clone, Default)]
pub struct LayerTree {
    layers: Vec<Layer>,
    viewport: Size,
}

impl LayerTree {
    /// Builds layers from a layout tree: one layer for the root and one for every
    /// scroll container, which is the minimum needed for correct scrolling.
    pub fn build(layout: &LayoutTree, viewport: Size) -> Self {
        let mut layers = Vec::new();
        if let Some(root) = layout.root {
            let root_rect = layout.boxes[root].rect;
            layers.push(Layer {
                id: LayerId(0),
                box_index: root,
                bounds: root_rect,
                scroll: (0.0, 0.0),
                z_index: 0,
                tiles: TileGrid::new(
                    256,
                    viewport.width.max(1.0) as u32,
                    viewport.height.max(1.0) as u32,
                ),
                opacity: layout.boxes[root].style.opacity(),
            });
        }
        for (index, b) in layout.boxes.iter().enumerate() {
            if b.clip.is_none() {
                continue;
            }
            let id = LayerId(layers.len());
            layers.push(Layer {
                id,
                box_index: index,
                bounds: b.rect,
                scroll: b.scroll_offset,
                z_index: b.z_index,
                tiles: TileGrid::new(
                    256,
                    b.rect.width.max(1.0) as u32,
                    b.rect.height.max(1.0) as u32,
                ),
                opacity: b.style.opacity(),
            });
        }
        Self { layers, viewport }
    }

    /// The layers, back to front.
    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    /// Mutable layers.
    pub fn layers_mut(&mut self) -> &mut [Layer] {
        &mut self.layers
    }

    /// Finds the layer generated by a layout box.
    pub fn layer_for_box(&self, box_index: usize) -> Option<&Layer> {
        self.layers.iter().find(|l| l.box_index == box_index)
    }

    /// Viewport used to build the tree.
    pub fn viewport(&self) -> Size {
        self.viewport
    }

    /// Invalidates everything (for example after a resize).
    pub fn resize(&mut self, viewport: Size) {
        self.viewport = viewport;
        for layer in &mut self.layers {
            layer.tiles = TileGrid::new(
                256,
                layer.bounds.width.max(1.0) as u32,
                layer.bounds.height.max(1.0) as u32,
            );
        }
    }

    /// Aggregated damage across all layers.
    pub fn take_damage(&mut self) -> Damage {
        let mut damage = Damage::new();
        for layer in &mut self.layers {
            let layer_damage = layer.take_damage();
            damage.add_all(&layer_damage);
        }
        damage
    }

    /// True when any layer needs repainting.
    pub fn has_damage(&self) -> bool {
        self.layers.iter().any(|l| l.tiles.dirty_count() > 0)
    }

    /// Total tile count.
    pub fn tile_count(&self) -> usize {
        self.layers.iter().map(|l| l.tiles.len()).sum()
    }

    /// Snapshots every layer in back-to-front paint order.
    ///
    /// Layers are sorted by `z_index` and then by id, so a backend can feed the
    /// result straight into a multi-layer compositor.
    pub fn composite_layers(&self) -> Vec<LayerComposite> {
        let mut layers: Vec<LayerComposite> =
            self.layers.iter().map(LayerComposite::from_layer).collect();
        layers.sort_by_key(|layer| (layer.z_index, layer.id.0));
        layers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_dom::Document;
    use browser_layout::layout;
    use browser_style::Origin;
    use browser_style::{StyleEngine, Viewport};

    fn tree() -> LayoutTree {
        let mut doc = Document::new();
        let html = doc.create_element("html");
        let body = doc.create_element("body");
        doc.append_child(doc.root(), html);
        doc.append_child(html, body);
        let div = doc.create_element("div");
        doc.append_child(body, div);
        doc.append_text(div, "content");
        let mut engine = StyleEngine::new(Viewport::default());
        engine.add_css(
            "div { height: 10px; } #s { overflow: scroll; height: 20px; }",
            Origin::Author,
        );
        let styled = engine.compute(&doc);
        layout(&doc, &styled, Size::default())
    }

    #[test]
    fn builds_root_layer_and_scroll_layers() {
        let layout = tree();
        let layers = LayerTree::build(
            &layout,
            Size {
                width: 800.0,
                height: 600.0,
            },
        );
        assert_eq!(layers.layers().len(), 1);
        assert!(layers.tile_count() > 0);
        assert!(layers.has_damage());
    }

    #[test]
    fn composite_snapshots_follow_stacking_order_and_transform() {
        let layout = tree();
        let mut layers = LayerTree::build(
            &layout,
            Size {
                width: 100.0,
                height: 100.0,
            },
        );
        // Give the root a scroll offset and a z-index to verify the snapshot.
        let root = &mut layers.layers_mut()[0];
        root.scroll = (5.0, 7.0);
        root.z_index = 2;
        root.opacity = 0.75;
        let snapshots = layers.composite_layers();
        assert_eq!(snapshots.len(), layers.layers().len());
        let first = &snapshots[0];
        assert_eq!(first.id, layers.layers()[0].id);
        assert_eq!(first.z_index, 2);
        assert!((first.opacity - 0.75).abs() < f32::EPSILON);
        let bounds = layers.layers()[0].bounds;
        assert_eq!(
            first.map_point(3.0, 4.0),
            (bounds.x - 5.0 + 3.0, bounds.y - 7.0 + 4.0)
        );
        assert_eq!(first.visible_bounds().x, bounds.x - 5.0);
    }

    #[test]
    fn damage_is_collected_and_cleared() {
        let layout = tree();
        let mut layers = LayerTree::build(
            &layout,
            Size {
                width: 100.0,
                height: 100.0,
            },
        );
        let damage = layers.take_damage();
        assert!(!damage.is_empty());
        assert!(!layers.has_damage());
    }
}

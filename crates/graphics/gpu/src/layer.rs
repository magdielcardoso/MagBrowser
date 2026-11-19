//! GPU-agnostic data model and math for the multi-layer compositor.
//!
//! The native presenter uploads every compositor layer as its own texture slice
//! and composites them back-to-front in a WGSL shader. Everything in this module
//! is plain CPU data: the transform matrices, blend equations, content hashes
//! and upload planner used by that shader. Keeping it GPU-free means the maths
//! is unit-testable without an adapter and the default workspace build never
//! links `wgpu`.
//!
//! A [`LayerFrame`] describes one layer for a single frame. [`LayerTextureCache`]
//! decides whether its pixels need to be transferred again: unchanged
//! (`layer id`, `content hash`) frames reuse their resident texture slice, and
//! changed frames only transfer the region reported by the dirty tracker.

use std::collections::HashMap;

use browser_layout::Rect;
use browser_paint::ImageBuffer;

use crate::device::{BlendMode, DirtyRect, DirtyTracker, GpuError, SubImageUpload, TextureUpload};

/// Default number of simultaneously resident layer textures.
///
/// Matches the depth of the texture array created by the native presenter.
pub const DEFAULT_LAYER_CAPACITY: usize = 32;

/// An affine 2D transform stored as a column-major 4×4 matrix.
///
/// The layout is identical to WGSL's `mat4x4<f32>`, so the same sixteen floats
/// can be written straight into a shader storage buffer. Only the 2D affine
/// subset (`[a c e; b d f]`) is used; the remaining entries stay at their
/// identity values.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(C)]
pub struct LayerTransform(pub [f32; 16]);

impl LayerTransform {
    /// Identity transform.
    pub const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0, //
    ]);

    /// Builds a transform from the six 2D affine coefficients.
    ///
    /// `a`/`b` are the x/y response to a local x, `c`/`d` to a local y, and
    /// `e`/`f` are the translation. This is the same convention as CSS's
    /// `matrix(a, b, c, d, e, f)`.
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self([
            a, b, 0.0, 0.0, //
            c, d, 0.0, 0.0, //
            0.0, 0.0, 1.0, 0.0, //
            e, f, 0.0, 1.0, //
        ])
    }

    /// Pure translation by `(x, y)` device pixels.
    pub const fn translation(x: f32, y: f32) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, x, y)
    }

    /// Pure scaling by `(x, y)`.
    pub const fn scaling(x: f32, y: f32) -> Self {
        Self::new(x, 0.0, 0.0, y, 0.0, 0.0)
    }

    /// Composes two transforms: applying `self` after `other`.
    pub fn compose(self, other: Self) -> Self {
        let mut out = [0.0_f32; 16];
        for column in 0..4 {
            for row in 0..4 {
                let mut value = 0.0;
                for k in 0..4 {
                    value += self.0[k * 4 + row] * other.0[column * 4 + k];
                }
                out[column * 4 + row] = value;
            }
        }
        Self(out)
    }

    /// Maps a source rectangle of `source_width × source_height` local pixels
    /// onto `dest` in device pixels.
    ///
    /// A zero-sized source axis is treated as one pixel to avoid dividing by
    /// zero; the resulting matrix is still finite.
    pub fn from_rect(source_width: u32, source_height: u32, dest: Rect) -> Self {
        let sx = dest.width / source_width.max(1) as f32;
        let sy = dest.height / source_height.max(1) as f32;
        Self::translation(dest.x, dest.y).compose(Self::scaling(sx, sy))
    }

    /// Transforms a local point `(x, y)` into device pixels.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.0;
        (m[0] * x + m[4] * y + m[12], m[1] * x + m[5] * y + m[13])
    }

    /// Inverts the affine part.
    ///
    /// Returns `None` when the matrix is singular (or non-finite), which the
    /// presenter treats as "do not sample this layer".
    pub fn inverse(&self) -> Option<Self> {
        let m = &self.0;
        let (a, b, c, d) = (m[0], m[1], m[4], m[5]);
        let determinant = a * d - b * c;
        if !determinant.is_finite() || determinant.abs() < 1e-12 {
            return None;
        }
        let ia = d / determinant;
        let ib = -b / determinant;
        let ic = -c / determinant;
        let id = a / determinant;
        let (e, f) = (m[12], m[13]);
        Some(Self::new(
            ia,
            ib,
            ic,
            id,
            -(ia * e + ic * f),
            -(ib * e + id * f),
        ))
    }

    /// The sixteen matrix floats, ready for a storage buffer write.
    pub const fn as_array(self) -> [f32; 16] {
        self.0
    }

    /// True when all sixteen entries are finite.
    pub fn is_finite(self) -> bool {
        self.0.iter().all(|value| value.is_finite())
    }
}

impl Default for LayerTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

/// Converts a straight-alpha RGBA sample to premultiplied RGBA.
///
/// Texture samples are straight alpha; the compositor accumulates
/// premultiplied colours so every blend mode shares one set of equations.
pub fn straight_to_premultiplied(rgba: [f32; 4]) -> [f32; 4] {
    let alpha = rgba[3];
    [rgba[0] * alpha, rgba[1] * alpha, rgba[2] * alpha, alpha]
}

/// Scales a premultiplied sample by a layer's opacity.
pub fn apply_opacity(rgba: [f32; 4], opacity: f32) -> [f32; 4] {
    let opacity = opacity.clamp(0.0, 1.0);
    [
        rgba[0] * opacity,
        rgba[1] * opacity,
        rgba[2] * opacity,
        rgba[3] * opacity,
    ]
}

fn unpremultiply(channel: f32, alpha: f32) -> f32 {
    if alpha > 1e-6 {
        (channel / alpha).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn overlay_channel(backdrop: f32, source: f32) -> f32 {
    if backdrop <= 0.5 {
        2.0 * backdrop * source
    } else {
        1.0 - 2.0 * (1.0 - backdrop) * (1.0 - source)
    }
}

impl BlendMode {
    /// Index used by the WGSL `blend_layer` switch.
    pub const fn shader_index(self) -> u32 {
        match self {
            Self::AlphaBlend | Self::Normal => 0,
            Self::Multiply => 1,
            Self::Screen => 2,
            Self::Overlay => 3,
            Self::Replace => 4,
        }
    }

    /// Blends a premultiplied `source` over a premultiplied `backdrop`.
    ///
    /// Both inputs and the result are RGBA with the colour channels already
    /// multiplied by alpha. Apart from [`BlendMode::Replace`] this is the W3C
    /// compositing equation
    /// `co = as·ab·B(cb, cs) + as·(1-ab)·cs + (1-as)·ab·cb`, which degenerates
    /// to plain source-over for `B(cb, cs) = cs`. The WGSL shader mirrors it
    /// exactly so CPU tests describe the GPU result.
    pub fn blend(self, source: [f32; 4], backdrop: [f32; 4]) -> [f32; 4] {
        if self == Self::Replace {
            return source;
        }
        let alpha_s = source[3].clamp(0.0, 1.0);
        let alpha_b = backdrop[3].clamp(0.0, 1.0);
        let alpha_o = (alpha_s + alpha_b - alpha_s * alpha_b).clamp(0.0, 1.0);
        let cs = [
            unpremultiply(source[0], alpha_s),
            unpremultiply(source[1], alpha_s),
            unpremultiply(source[2], alpha_s),
        ];
        let cb = [
            unpremultiply(backdrop[0], alpha_b),
            unpremultiply(backdrop[1], alpha_b),
            unpremultiply(backdrop[2], alpha_b),
        ];
        let blended = match self {
            Self::AlphaBlend | Self::Normal => cs,
            Self::Multiply => [cb[0] * cs[0], cb[1] * cs[1], cb[2] * cs[2]],
            Self::Screen => [
                cb[0] + cs[0] - cb[0] * cs[0],
                cb[1] + cs[1] - cb[1] * cs[1],
                cb[2] + cs[2] - cb[2] * cs[2],
            ],
            Self::Overlay => [
                overlay_channel(cb[0], cs[0]),
                overlay_channel(cb[1], cs[1]),
                overlay_channel(cb[2], cs[2]),
            ],
            Self::Replace => unreachable!("handled above"),
        };
        let bs = alpha_s * alpha_b;
        let bs_only = alpha_s * (1.0 - alpha_b);
        let bb_only = (1.0 - alpha_s) * alpha_b;
        let mut out = [0.0_f32; 4];
        for channel in 0..3 {
            out[channel] = (blended[channel] * bs + cs[channel] * bs_only + cb[channel] * bb_only)
                .clamp(0.0, 1.0);
        }
        out[3] = alpha_o;
        out
    }
}

/// FNV-1a hash of raw RGBA bytes.
///
/// Fast, dependency-free and deterministic across builds: a content hash only
/// has to change when pixels change, never to be cryptographically secure.
pub fn content_hash(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Hashes only the pixels inside `dirty`, or the whole image when it is `None`.
pub fn dirty_content_hash(pixels: &[u8], width: u32, height: u32, dirty: Option<DirtyRect>) -> u64 {
    match dirty.filter(|rect| !rect.is_empty()) {
        Some(rect) => rect
            .extract(pixels, width, height)
            .map_or_else(|_| content_hash(pixels), |sub| content_hash(&sub.bytes)),
        None => content_hash(pixels),
    }
}

/// One compositor layer's contribution to a single frame.
///
/// `pixels` is tightly packed straight-alpha RGBA in layer-local coordinates.
/// The presenter uploads `dirty` (or the full image) into that layer's texture
/// slice, then draws it with `opacity`, `transform` and `blend`.
#[derive(Debug, Clone)]
pub struct LayerFrame<'a> {
    /// Stable compositor layer identifier.
    pub id: u64,
    /// Tightly packed RGBA pixels of the layer.
    pub pixels: &'a [u8],
    /// Width of `pixels` in pixels.
    pub width: u32,
    /// Height of `pixels` in pixels.
    pub height: u32,
    /// Destination rectangle in device pixels (used by the default transform).
    pub dest: Rect,
    /// Opacity in `[0, 1]`, applied before blending.
    pub opacity: f32,
    /// Layer-local pixels → device pixels.
    pub transform: LayerTransform,
    /// How this layer combines with the layers below it.
    pub blend: BlendMode,
    /// Region of `pixels` that changed, in layer-local pixels.
    pub dirty: Option<DirtyRect>,
    /// Optional caller-supplied content hash; computed from pixels when `None`.
    pub content_hash: Option<u64>,
}

impl<'a> LayerFrame<'a> {
    /// Builds a fully opaque, normally blended frame covering `dest`.
    pub fn new(id: u64, image: &'a ImageBuffer, dest: Rect) -> Self {
        let transform = LayerTransform::from_rect(image.width, image.height, dest);
        Self {
            id,
            pixels: &image.pixels,
            width: image.width,
            height: image.height,
            dest,
            opacity: 1.0,
            transform,
            blend: BlendMode::Normal,
            dirty: Some(DirtyRect::new(0, 0, image.width, image.height)),
            content_hash: None,
        }
    }

    /// Overrides the layer opacity.
    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity;
        self
    }

    /// Overrides the layer blend mode.
    pub fn with_blend(mut self, blend: BlendMode) -> Self {
        self.blend = blend;
        self
    }

    /// Overrides the layer transform.
    pub fn with_transform(mut self, transform: LayerTransform) -> Self {
        self.transform = transform;
        self
    }

    /// Sets the changed region; `None` requests a full upload.
    pub fn with_dirty(mut self, dirty: Option<DirtyRect>) -> Self {
        self.dirty = dirty;
        self
    }

    /// Consumes a [`DirtyTracker`]'s bounding box as the changed region.
    ///
    /// Painters mark changed rects on their tracker while rasterizing a layer;
    /// calling this before presentation makes the texture cache transfer only
    /// that bounding box. An empty tracker leaves `dirty` as `None` (full
    /// upload), which is the safe default.
    pub fn take_dirty_from(mut self, tracker: &mut DirtyTracker) -> Self {
        self.dirty = tracker.take();
        self
    }

    /// Overrides the content hash (useful when the painter already has one).
    pub fn with_content_hash(mut self, hash: u64) -> Self {
        self.content_hash = Some(hash);
        self
    }

    /// Content hash actually used by the cache.
    pub fn resolved_hash(&self) -> u64 {
        self.content_hash
            .unwrap_or_else(|| dirty_content_hash(self.pixels, self.width, self.height, self.dirty))
    }

    /// True when the layer has no pixels to draw.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Identity of a resident layer texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayerCacheKey {
    /// Stable layer identifier.
    pub layer_id: u64,
    /// Hash of the layer's pixel content.
    pub content_hash: u64,
    /// Width of the uploaded content.
    pub width: u32,
    /// Height of the uploaded content.
    pub height: u32,
}

/// What the presenter should do with one layer's pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadAction {
    /// The texture slice already holds this content.
    Reuse {
        /// Texture array slice to sample.
        slot: usize,
    },
    /// Write `sub` into the texture slice before drawing.
    Upload {
        /// Texture array slice to write and sample.
        slot: usize,
        /// Region and tight RGBA bytes to transfer.
        sub: SubImageUpload,
    },
}

#[derive(Debug, Clone, Copy)]
struct LayerSlot {
    key: Option<LayerCacheKey>,
    last_used: u64,
}

/// Plans layer texture uploads so unchanged layers are never re-transferred.
///
/// Keys are `(layer id, content hash)`; the layer id keeps a slot stable when
/// pixels change, while the hash decides whether a transfer is needed at all.
/// When a layer id changes content only inside a dirty rectangle, the resulting
/// [`UploadAction::Upload`] carries exactly that sub-image. Slots are recycled
/// least-recently-used once the cache is full.
#[derive(Debug)]
pub struct LayerTextureCache {
    slots: Vec<LayerSlot>,
    capacity: usize,
    by_id: HashMap<u64, usize>,
    clock: u64,
    uploads: usize,
    reuses: usize,
}

impl LayerTextureCache {
    /// Creates a cache holding at most `capacity` layer textures.
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: Vec::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
            by_id: HashMap::new(),
            clock: 0,
            uploads: 0,
            reuses: 0,
        }
    }

    /// Maximum number of resident layers.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of layers with a resident texture.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// True when no layer is resident.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Number of planned pixel uploads so far.
    pub fn uploads(&self) -> usize {
        self.uploads
    }

    /// Number of frames that reused an existing texture.
    pub fn reuses(&self) -> usize {
        self.reuses
    }

    /// Forgets every layer, forcing full uploads on the next frame.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.by_id.clear();
        // Counters are retained: they describe the cache's lifetime.
    }

    /// Decides whether `frame` needs a texture write and returns its slot.
    pub fn plan(&mut self, frame: &LayerFrame<'_>) -> Result<UploadAction, GpuError> {
        self.clock = self.clock.wrapping_add(1);
        if frame.width == 0 || frame.height == 0 {
            return Err(GpuError::InvalidData {
                expected: 0,
                actual: frame.pixels.len(),
            });
        }
        let key = LayerCacheKey {
            layer_id: frame.id,
            content_hash: frame.resolved_hash(),
            width: frame.width,
            height: frame.height,
        };
        if let Some(&slot) = self.by_id.get(&frame.id) {
            self.slots[slot].last_used = self.clock;
            if self.slots[slot].key == Some(key) {
                self.reuses += 1;
                return Ok(UploadAction::Reuse { slot });
            }
            let size_changed = self.slots[slot]
                .key
                .is_some_and(|old| (old.width, old.height) != (frame.width, frame.height));
            let sub = self.sub_image(frame, size_changed)?;
            self.slots[slot].key = Some(key);
            self.uploads += 1;
            return Ok(UploadAction::Upload { slot, sub });
        }
        let slot = self.allocate_slot();
        let sub = self.sub_image(frame, false)?;
        self.slots[slot].key = Some(key);
        self.by_id.insert(frame.id, slot);
        self.uploads += 1;
        Ok(UploadAction::Upload { slot, sub })
    }

    fn sub_image(
        &self,
        frame: &LayerFrame<'_>,
        force_full: bool,
    ) -> Result<SubImageUpload, GpuError> {
        let full = DirtyRect::new(0, 0, frame.width, frame.height);
        let rect = if force_full {
            full
        } else {
            frame
                .dirty
                .filter(|rect| !rect.is_empty())
                .unwrap_or(full)
                .clamp(frame.width, frame.height)
        };
        let rect = if rect.is_empty() { full } else { rect };
        rect.extract(frame.pixels, frame.width, frame.height)
    }

    fn allocate_slot(&mut self) -> usize {
        if let Some(index) = self.slots.iter().position(|slot| slot.key.is_none()) {
            return index;
        }
        if self.slots.len() < self.capacity() {
            self.slots.push(LayerSlot {
                key: None,
                last_used: self.clock,
            });
            return self.slots.len() - 1;
        }
        let victim = self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, slot)| slot.last_used)
            .map_or(0, |(index, _)| index);
        if let Some(old) = self.slots[victim].key {
            self.by_id.remove(&old.layer_id);
        }
        self.slots[victim].last_used = self.clock;
        victim
    }
}

/// Extracts row-padded bytes suitable for `queue.write_texture`.
///
/// Reuses [`TextureUpload`] so dirty sub-images obey the same 256-byte row
/// alignment as full-frame uploads.
pub fn padded_upload(sub: &SubImageUpload) -> Result<TextureUpload, GpuError> {
    TextureUpload::new(sub.width, sub.height, &sub.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::COPY_BYTES_PER_ROW_ALIGNMENT;

    fn image(width: u32, height: u32, value: u8) -> ImageBuffer {
        ImageBuffer {
            width,
            height,
            pixels: vec![value; width as usize * height as usize * 4],
        }
    }

    #[test]
    fn transform_maps_points_and_composes() {
        let translation = LayerTransform::translation(10.0, 20.0);
        let scale = LayerTransform::scaling(2.0, 3.0);
        assert_eq!(translation.apply(0.0, 0.0), (10.0, 20.0));
        assert_eq!(scale.apply(4.0, 5.0), (8.0, 15.0));
        assert_eq!(translation.compose(scale).apply(4.0, 5.0), (18.0, 35.0));
        assert_eq!(LayerTransform::IDENTITY.apply(7.0, 9.0), (7.0, 9.0));
        assert!(LayerTransform::IDENTITY.is_finite());
    }

    #[test]
    fn transform_from_rect_maps_source_corners_to_dest() {
        let transform = LayerTransform::from_rect(20, 10, Rect::new(100.0, 200.0, 40.0, 30.0));
        assert_eq!(transform.apply(0.0, 0.0), (100.0, 200.0));
        assert_eq!(transform.apply(20.0, 10.0), (140.0, 230.0));
        assert_eq!(transform.apply(10.0, 5.0), (120.0, 215.0));
    }

    #[test]
    fn transform_inverse_round_trips_and_rejects_singular() {
        let transform = LayerTransform::new(2.0, 0.5, -0.25, 3.0, 7.0, -4.0);
        let inverse = transform.inverse().expect("invertible transform");
        let (x, y) = transform.apply(11.0, 6.0);
        let (back_x, back_y) = inverse.apply(x, y);
        assert!((back_x - 11.0).abs() < 1e-4);
        assert!((back_y - 6.0).abs() < 1e-4);
        assert_eq!(LayerTransform::scaling(0.0, 1.0).inverse(), None);
    }

    #[test]
    fn premultiplication_and_opacity_match_shader_input() {
        assert_eq!(
            straight_to_premultiplied([1.0, 0.5, 0.0, 0.5]),
            [0.5, 0.25, 0.0, 0.5]
        );
        assert_eq!(
            apply_opacity([1.0, 0.0, 0.0, 1.0], 0.25),
            [0.25, 0.0, 0.0, 0.25]
        );
        assert_eq!(apply_opacity([1.0, 1.0, 1.0, 1.0], 4.0)[3], 1.0);
    }

    #[test]
    fn blend_modes_match_compositing_equations() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let blue = [0.0, 0.0, 1.0, 1.0];
        assert_eq!(BlendMode::Normal.blend(red, blue), red);
        assert_eq!(BlendMode::AlphaBlend.blend(red, blue), red);
        assert_eq!(BlendMode::Replace.blend(red, blue), red);

        // red × blue = black, red screen blue = magenta.
        assert_eq!(BlendMode::Multiply.blend(red, blue), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(BlendMode::Screen.blend(red, blue), [1.0, 0.0, 1.0, 1.0]);

        // A fully opaque source over a translucent backdrop keeps the source's
        // straight colour in the premultiplied result.
        let half = straight_to_premultiplied([1.0, 1.0, 1.0, 0.5]);
        assert_eq!(BlendMode::Normal.blend(red, half), [1.0, 0.0, 0.0, 1.0]);

        // overlay: backdrop 0.2 (< 0.5) with source 1.0 gives 2·0.2·1.0.
        let grey = [0.2, 0.2, 0.2, 1.0];
        let overlaid = BlendMode::Overlay.blend(red, grey);
        assert!((overlaid[0] - 0.4).abs() < 1e-6, "{overlaid:?}");
        // The source has no green or blue, so overlay leaves those channels 0.
        assert_eq!(overlaid[1], 0.0);
        assert_eq!(overlaid[2], 0.0);
        assert_eq!(overlaid[3], 1.0);
    }

    #[test]
    fn shader_indices_are_stable() {
        assert_eq!(BlendMode::AlphaBlend.shader_index(), 0);
        assert_eq!(BlendMode::Normal.shader_index(), 0);
        assert_eq!(BlendMode::Multiply.shader_index(), 1);
        assert_eq!(BlendMode::Screen.shader_index(), 2);
        assert_eq!(BlendMode::Overlay.shader_index(), 3);
        assert_eq!(BlendMode::Replace.shader_index(), 4);
    }

    #[test]
    fn content_hash_tracks_pixels_and_dirty_regions() {
        let first = image(2, 2, 10);
        let mut second = first.clone();
        second.pixels[0] = 11;
        assert_eq!(content_hash(&first.pixels), content_hash(&first.pixels));
        assert_ne!(content_hash(&first.pixels), content_hash(&second.pixels));
        // The changed pixel lies inside the dirty rect, so the hashes differ,
        // but a hash restricted to a clean region must stay stable.
        assert_ne!(
            dirty_content_hash(&first.pixels, 2, 2, Some(DirtyRect::new(0, 0, 1, 1))),
            dirty_content_hash(&second.pixels, 2, 2, Some(DirtyRect::new(0, 0, 1, 1)))
        );
        assert_eq!(
            dirty_content_hash(&first.pixels, 2, 2, Some(DirtyRect::new(1, 1, 1, 1))),
            dirty_content_hash(&second.pixels, 2, 2, Some(DirtyRect::new(1, 1, 1, 1)))
        );
    }

    #[test]
    fn cache_reuses_unchanged_and_uploads_dirty_regions() {
        let first = image(4, 4, 7);
        let frame = LayerFrame::new(9, &first, Rect::new(0.0, 0.0, 4.0, 4.0));
        let mut cache = LayerTextureCache::new(2);
        match cache.plan(&frame).unwrap() {
            UploadAction::Upload { slot, sub } => {
                assert_eq!(slot, 0);
                assert_eq!(sub.rect(), DirtyRect::new(0, 0, 4, 4));
                assert_eq!(sub.bytes.len(), 64);
            }
            other => panic!("expected a full upload, got {other:?}"),
        }
        assert_eq!(cache.plan(&frame).unwrap(), UploadAction::Reuse { slot: 0 });
        assert_eq!(cache.uploads(), 1);
        assert_eq!(cache.reuses(), 1);
        assert_eq!(cache.len(), 1);

        let mut changed = first.clone();
        changed.pixels[5 * 4..5 * 4 + 4].copy_from_slice(&[1, 2, 3, 4]);
        let dirty = DirtyRect::new(1, 1, 1, 1);
        let changed_frame = LayerFrame::new(9, &changed, Rect::ZERO)
            .with_dirty(Some(dirty))
            .with_transform(LayerTransform::IDENTITY);
        match cache.plan(&changed_frame).unwrap() {
            UploadAction::Upload { slot, sub } => {
                assert_eq!(slot, 0, "changed content must reuse the layer's slot");
                assert_eq!(sub.rect(), dirty);
                assert_eq!(sub.bytes, [1, 2, 3, 4]);
            }
            other => panic!("expected a dirty upload, got {other:?}"),
        }
        assert_eq!(
            cache.plan(&changed_frame).unwrap(),
            UploadAction::Reuse { slot: 0 }
        );
    }

    #[test]
    fn cache_assigns_distinct_slots_and_evicts_lru() {
        let image = image(2, 2, 3);
        let a = LayerFrame::new(1, &image, Rect::ZERO);
        let b = LayerFrame::new(2, &image, Rect::ZERO);
        let mut cache = LayerTextureCache::new(2);
        assert!(matches!(
            cache.plan(&a).unwrap(),
            UploadAction::Upload { slot: 0, .. }
        ));
        assert!(matches!(
            cache.plan(&b).unwrap(),
            UploadAction::Upload { slot: 1, .. }
        ));
        assert_eq!(cache.plan(&a).unwrap(), UploadAction::Reuse { slot: 0 });

        let c = LayerFrame::new(3, &image, Rect::ZERO);
        match cache.plan(&c).unwrap() {
            UploadAction::Upload { slot, .. } => {
                assert_eq!(slot, 1, "layer b is least recently used")
            }
            other => panic!("expected an upload, got {other:?}"),
        }
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_uploads_full_image_when_layer_size_changes() {
        let small = image(2, 2, 4);
        let frame =
            LayerFrame::new(5, &small, Rect::ZERO).with_dirty(Some(DirtyRect::new(0, 0, 1, 1)));
        let mut cache = LayerTextureCache::new(1);
        assert!(matches!(
            cache.plan(&frame).unwrap(),
            UploadAction::Upload { sub, .. } if sub.rect() == DirtyRect::new(0, 0, 1, 1)
        ));

        let large = image(4, 4, 4);
        let resized =
            LayerFrame::new(5, &large, Rect::ZERO).with_dirty(Some(DirtyRect::new(0, 0, 1, 1)));
        assert!(matches!(
            cache.plan(&resized).unwrap(),
            UploadAction::Upload { slot: 0, sub } if sub.rect() == DirtyRect::new(0, 0, 4, 4)
        ));
    }

    #[test]
    fn dirty_tracker_bounds_drive_sub_image_uploads() {
        let pixels = image(4, 4, 9);
        let mut tracker = DirtyTracker::new();
        tracker.mark(DirtyRect::new(1, 1, 2, 2));
        tracker.mark(DirtyRect::new(2, 3, 1, 1));
        let frame = LayerFrame::new(3, &pixels, Rect::ZERO).take_dirty_from(&mut tracker);
        assert_eq!(frame.dirty, Some(DirtyRect::new(1, 1, 2, 3)));
        assert!(tracker.is_empty());

        let mut cache = LayerTextureCache::new(1);
        match cache.plan(&frame).unwrap() {
            UploadAction::Upload { sub, .. } => {
                assert_eq!(sub.rect(), DirtyRect::new(1, 1, 2, 3));
            }
            other => panic!("expected an upload, got {other:?}"),
        }
    }

    #[test]
    fn padded_upload_aligns_dirty_rows() {
        let pixels: Vec<u8> = (0..64).collect();
        let sub = DirtyRect::new(1, 1, 2, 2).extract(&pixels, 4, 4).unwrap();
        let upload = padded_upload(&sub).unwrap();
        assert_eq!(upload.bytes_per_row, COPY_BYTES_PER_ROW_ALIGNMENT);
        assert_eq!(upload.tight_bytes(), sub.bytes);
    }
}

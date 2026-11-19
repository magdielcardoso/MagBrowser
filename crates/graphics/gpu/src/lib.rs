//! GPU backend abstraction.
//!
//! The display list is the single input shared by the software rasterizer and the
//! GPU path. WGSL shaders are embedded here; [`SoftwareDevice`] is the reference
//! implementation used for headless rendering. [`GlyphAtlas`] paints smooth,
//! anti-aliased window chrome text, and the optional `wgpu-backend`
//! feature adds a native surface presenter without changing the [`Device`] API.
//!
//! For smoother presentation the crate also exposes row-padded texture uploads
//! ([`TextureUpload`], [`padded_row_bytes`]), a two-slot frame [`DoubleBuffer`],
//! and a dirty-rect path ([`DirtyRect`], [`DirtyTracker`], [`SubImageUpload`])
//! so only changed pixels are transferred.
//!
//! The GPU-free [`layer`] module models a real multi-layer compositor:
//! [`LayerFrame`] describes one layer, [`LayerTransform`] and [`BlendMode`]
//! supply its matrix and blend equation, and [`LayerTextureCache`] avoids
//! re-uploading unchanged layers. With the optional `wgpu-backend` feature the
//! native presenter draws them all with one WGSL shader
//! ([`LAYER_SHADER`](device::LAYER_SHADER)); the default build stays GPU-free.
//!
//! ```
//! use browser_css::Color;
//! use browser_gpu::{BlendMode, DirtyRect, LayerFrame, LayerTextureCache, LAYER_SHADER};
//! use browser_layout::Rect;
//! use browser_paint::ImageBuffer;
//!
//! let image = ImageBuffer::filled(2, 2, Color::rgb(255, 0, 0));
//! let frame = LayerFrame::new(1, &image, Rect::new(0.0, 0.0, 2.0, 2.0))
//!     .with_blend(BlendMode::Multiply)
//!     .with_dirty(Some(DirtyRect::new(0, 0, 1, 1)));
//! let mut cache = LayerTextureCache::new(2);
//! assert!(cache.plan(&frame).is_ok()); // first frame uploads
//! assert!(cache.plan(&frame).is_ok()); // second frame reuses the texture
//! assert!(LAYER_SHADER.contains("blend_layer"));
//! ```

#![forbid(unsafe_code)]

pub mod device;
pub mod glyph_atlas;
pub mod layer;
#[cfg(feature = "wgpu-backend")]
pub mod native;

pub use device::{
    BlendMode, BufferId, COPY_BYTES_PER_ROW_ALIGNMENT, Device, DirtyRect, DirtyTracker,
    DoubleBuffer, FrameDoubleBuffer, GpuError, IMAGE_SHADER, LAYER_SHADER, PipelineDescriptor,
    RECT_SHADER, RectVertex, SoftwareDevice, SubImageUpload, TextureFormat, TextureId,
    TextureUpload, align_up, padded_row_bytes,
};
pub use glyph_atlas::GlyphAtlas;
pub use layer::{
    DEFAULT_LAYER_CAPACITY, LayerCacheKey, LayerFrame, LayerTextureCache, LayerTransform,
    UploadAction, apply_opacity, content_hash, dirty_content_hash, padded_upload,
    straight_to_premultiplied,
};
#[cfg(feature = "wgpu-backend")]
pub use native::{NativeDevice, NativeError, NativePresenter};

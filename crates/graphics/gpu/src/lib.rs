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

#![forbid(unsafe_code)]

pub mod device;
pub mod glyph_atlas;
#[cfg(feature = "wgpu-backend")]
pub mod native;

pub use device::{
    BlendMode, BufferId, COPY_BYTES_PER_ROW_ALIGNMENT, Device, DirtyRect, DirtyTracker,
    DoubleBuffer, FrameDoubleBuffer, GpuError, IMAGE_SHADER, PipelineDescriptor, RECT_SHADER,
    RectVertex, SoftwareDevice, SubImageUpload, TextureFormat, TextureId, TextureUpload, align_up,
    padded_row_bytes,
};
pub use glyph_atlas::GlyphAtlas;
#[cfg(feature = "wgpu-backend")]
pub use native::{NativeDevice, NativeError, NativePresenter};

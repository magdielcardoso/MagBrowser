//! GPU backend abstraction.
//!
//! The display list is the single input shared by the software rasterizer and the
//! GPU path. WGSL shaders are embedded here; [`SoftwareDevice`] is the reference
//! implementation used for headless rendering. [`GlyphAtlas`] provides the
//! bitmap-font texture used by window chrome, and the optional `wgpu-backend`
//! feature adds a native surface presenter without changing the [`Device`] API.

#![forbid(unsafe_code)]

pub mod device;
pub mod glyph_atlas;
#[cfg(feature = "wgpu-backend")]
pub mod native;

pub use device::{
    BlendMode, BufferId, Device, GpuError, IMAGE_SHADER, PipelineDescriptor, RECT_SHADER,
    RectVertex, SoftwareDevice, TextureFormat, TextureId,
};
pub use glyph_atlas::GlyphAtlas;
#[cfg(feature = "wgpu-backend")]
pub use native::{NativeDevice, NativeError, NativePresenter};

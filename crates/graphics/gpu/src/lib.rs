//! GPU backend abstraction.
//!
//! The display list is the single input shared by the software rasterizer and the
//! GPU path. WGSL shaders are embedded here; [`SoftwareDevice`] is the reference
//! implementation used for headless rendering.

#![forbid(unsafe_code)]

pub mod device;

pub use device::{
    BlendMode, BufferId, Device, GpuError, IMAGE_SHADER, PipelineDescriptor, RECT_SHADER,
    RectVertex, SoftwareDevice, TextureFormat, TextureId,
};

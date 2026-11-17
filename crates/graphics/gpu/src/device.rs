//! GPU abstraction: pipeline descriptions, surfaces and a reference device.
//!
//! The engine targets `wgpu` for hardware acceleration, but the backend is kept
//! behind a small trait ([`Device`]) so that headless builds (tests, screenshots,
//! CI) use the CPU rasterizer with exactly the same input: a display list.
//! The WGSL shaders used by the hardware path live in `shaders/`.
//!
//! **Status:** the software device is implemented and used by
//! `apps/browser-headless`. The native `wgpu` device is not implemented yet; see
//! `docs/roadmap.md`.

use browser_css::Color;
use browser_layout::Size;
use browser_paint::{DisplayList, ImageBuffer, Rasterizer};

/// Pixel formats the surface layer understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    /// 8-bit RGBA, the format used by the software rasterizer and PNG writer.
    Rgba8,
    /// 8-bit BGRA, the preferred format on many Metal/Vulkan surfaces.
    Bgra8,
}

/// Blending modes available to pipelines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// Source-over alpha blending.
    #[default]
    AlphaBlend,
    /// Replace the destination.
    Replace,
}

/// A static description of a render pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineDescriptor {
    /// Human-readable pipeline name.
    pub name: String,
    /// Target colour format.
    pub format: TextureFormat,
    /// Blend mode.
    pub blend: BlendMode,
    /// WGSL entry point name.
    pub entry_point: String,
    /// Shader source embedded in the binary.
    pub shader: &'static str,
}

/// Vertex data for one rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[repr(C)]
pub struct RectVertex {
    /// Position in device pixels.
    pub position: [f32; 2],
    /// Local UV.
    pub uv: [f32; 2],
    /// Premultiplied RGBA colour.
    pub color: [f32; 4],
}

/// A GPU texture handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureId(pub usize);

/// A GPU buffer handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BufferId(pub usize);

/// Errors surfaced by device operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    /// The surface has not been configured.
    NotConfigured,
    /// The requested texture does not exist.
    UnknownTexture(TextureId),
    /// A shader could not be compiled.
    ShaderError(String),
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("surface is not configured"),
            Self::UnknownTexture(id) => write!(f, "unknown texture {}", id.0),
            Self::ShaderError(msg) => write!(f, "shader error: {msg}"),
        }
    }
}

impl std::error::Error for GpuError {}

/// Rectangle shader.
pub const RECT_SHADER: &str = include_str!("shaders/rect.wgsl");
/// Image shader.
pub const IMAGE_SHADER: &str = include_str!("shaders/image.wgsl");

/// The engine's device interface.
pub trait Device {
    /// Human-readable backend name.
    fn name(&self) -> &str;

    /// Renders a display list into an image buffer.
    fn render(&mut self, list: &DisplayList, viewport: Size) -> Result<ImageBuffer, GpuError>;

    /// Creates a texture and returns its handle.
    fn create_texture(&mut self, width: u32, height: u32) -> TextureId;

    /// Uploads pixel data into a texture.
    fn upload_texture(
        &mut self,
        id: TextureId,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), GpuError>;
}

/// CPU device implementing the same interface as the hardware path.
#[derive(Debug, Clone)]
pub struct SoftwareDevice {
    textures: Vec<(u32, u32, Vec<u8>)>,
    buffers: Vec<Vec<u8>>,
    background: Color,
    pipeline: PipelineDescriptor,
}

impl Default for SoftwareDevice {
    fn default() -> Self {
        Self::new(Color::rgb(255, 255, 255))
    }
}

impl SoftwareDevice {
    /// Creates a device with a background colour.
    pub fn new(background: Color) -> Self {
        Self {
            textures: Vec::new(),
            buffers: Vec::new(),
            background,
            pipeline: PipelineDescriptor {
                name: "rect".to_string(),
                format: TextureFormat::Rgba8,
                blend: BlendMode::AlphaBlend,
                entry_point: "vs_main".to_string(),
                shader: RECT_SHADER,
            },
        }
    }

    /// The active pipeline description.
    pub fn pipeline(&self) -> &PipelineDescriptor {
        &self.pipeline
    }

    /// Allocates a vertex buffer.
    pub fn create_buffer(&mut self, bytes: Vec<u8>) -> BufferId {
        self.buffers.push(bytes);
        BufferId(self.buffers.len() - 1)
    }

    /// Reads a texture back as RGBA.
    pub fn read_texture(&self, id: TextureId) -> Option<&[u8]> {
        self.textures.get(id.0).map(|(_, _, data)| data.as_slice())
    }

    /// Number of live textures.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }
}

impl Device for SoftwareDevice {
    fn name(&self) -> &str {
        "software"
    }

    fn render(&mut self, list: &DisplayList, viewport: Size) -> Result<ImageBuffer, GpuError> {
        Ok(Rasterizer::new(viewport, self.background).execute(list))
    }

    fn create_texture(&mut self, width: u32, height: u32) -> TextureId {
        self.textures
            .push((width, height, vec![0; width as usize * height as usize * 4]));
        TextureId(self.textures.len() - 1)
    }

    fn upload_texture(
        &mut self,
        id: TextureId,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<(), GpuError> {
        let Some(texture) = self.textures.get_mut(id.0) else {
            return Err(GpuError::UnknownTexture(id));
        };
        if texture.0 != width || texture.1 != height {
            texture.0 = width;
            texture.1 = height;
            texture.2 = vec![0; width as usize * height as usize * 4];
        }
        let len = rgba.len().min(texture.2.len());
        texture.2[..len].copy_from_slice(&rgba[..len]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use browser_layout::Rect;
    use browser_paint::DisplayCommand;

    #[test]
    fn shaders_are_embedded_and_non_empty() {
        assert!(RECT_SHADER.contains("fn vs_main"));
        assert!(IMAGE_SHADER.contains("fn vs_main"));
        assert_eq!(RECT_SHADER, include_str!("shaders/rect.wgsl"));
    }

    #[test]
    fn software_device_renders_display_lists() {
        let mut device = SoftwareDevice::new(Color::rgb(255, 255, 255));
        let mut list = DisplayList::new();
        list.push(DisplayCommand::FillRect {
            rect: Rect::new(0.0, 0.0, 8.0, 8.0),
            color: Color::rgb(255, 0, 0),
        });
        let image = device
            .render(
                &list,
                Size {
                    width: 16.0,
                    height: 16.0,
                },
            )
            .unwrap();
        assert_eq!(image.pixel(2, 2), [255, 0, 0, 255]);
        assert_eq!(device.name(), "software");
    }

    #[test]
    fn textures_round_trip() {
        let mut device = SoftwareDevice::default();
        let id = device.create_texture(2, 2);
        let data = vec![7_u8; 16];
        device.upload_texture(id, 2, 2, &data).unwrap();
        assert_eq!(device.read_texture(id).unwrap()[0], 7);
        assert_eq!(device.texture_count(), 1);
        assert_eq!(
            device.upload_texture(TextureId(99), 1, 1, &[]),
            Err(GpuError::UnknownTexture(TextureId(99)))
        );
    }
}

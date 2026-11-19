//! GPU abstraction: pipeline descriptions, surfaces and a reference device.
//!
//! The engine targets `wgpu` for hardware acceleration, but the backend is kept
//! behind a small trait ([`Device`]) so that headless builds (tests, screenshots,
//! CI) use the CPU rasterizer with exactly the same input: a display list.
//! The WGSL shaders used by the hardware path live in `shaders/`.
//!
//! Besides the display-list boundary, this module offers the small data helpers
//! a presenter needs for smooth frames: [`TextureUpload`] pads rows to the byte
//! alignment required by texture-copy APIs, [`DoubleBuffer`] alternates two frame
//! slots, and [`DirtyRect`]/[`DirtyTracker`]/[`SubImageUpload`] describe and
//! extract only the pixels that actually changed.
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
///
/// The first two variants predate the layer compositor; `Normal` is the name
/// used by the compositor and behaves identically to `AlphaBlend`. The maths
/// for all modes lives in [`crate::layer::BlendMode::blend`] and is mirrored by
/// the WGSL `blend_layer` function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BlendMode {
    /// Source-over alpha blending (legacy name, identical to [`Self::Normal`]).
    #[default]
    AlphaBlend,
    /// Replace the destination.
    Replace,
    /// Source-over alpha blending.
    Normal,
    /// Multiply the source and backdrop colours.
    Multiply,
    /// Screen the source and backdrop colours.
    Screen,
    /// Overlay the source over the backdrop.
    Overlay,
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
    /// Pixel data had the wrong length for the requested dimensions.
    InvalidData {
        /// Number of bytes the operation expected.
        expected: usize,
        /// Number of bytes that were supplied.
        actual: usize,
    },
    /// The region falls outside the destination texture.
    OutOfBounds {
        /// Origin and size of the region.
        region: DirtyRect,
        /// Size of the destination texture (`width`, `height`).
        texture: (u32, u32),
    },
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("surface is not configured"),
            Self::UnknownTexture(id) => write!(f, "unknown texture {}", id.0),
            Self::ShaderError(msg) => write!(f, "shader error: {msg}"),
            Self::InvalidData { expected, actual } => {
                write!(
                    f,
                    "invalid pixel data: expected {expected} bytes, got {actual}"
                )
            }
            Self::OutOfBounds { region, texture } => write!(
                f,
                "region ({},{}) {}x{} is outside the {}x{} texture",
                region.x, region.y, region.width, region.height, texture.0, texture.1
            ),
        }
    }
}

impl std::error::Error for GpuError {}

/// Rectangle shader.
pub const RECT_SHADER: &str = include_str!("shaders/rect.wgsl");
/// Image shader.
pub const IMAGE_SHADER: &str = include_str!("shaders/image.wgsl");
/// Multi-layer compositor shader (opacity, transform and blend modes).
pub const LAYER_SHADER: &str = include_str!("shaders/layer.wgsl");

/// Byte alignment that `wgpu` requires for texture row copies.
///
/// `queue.write_texture` expects `bytes_per_row` to be a multiple of 256. Use
/// [`TextureUpload`] to turn tightly packed RGBA pixels into an aligned upload.
pub const COPY_BYTES_PER_ROW_ALIGNMENT: u32 = 256;

/// Rounds `value` up to the next multiple of `alignment`.
///
/// `alignment` must be a power of two (debug builds assert this). The result is
/// computed with wrapping arithmetic so oversized values saturate at the type
/// boundary instead of panicking.
pub fn align_up(value: u32, alignment: u32) -> u32 {
    debug_assert!(
        alignment.is_power_of_two(),
        "alignment must be a power of two"
    );
    if alignment <= 1 {
        return value;
    }
    value.wrapping_add(alignment - 1) & !(alignment - 1)
}

/// Number of bytes a row occupies once padded to `alignment`.
///
/// This is the value to pass as `bytes_per_row` to a texture-copy API.
pub fn padded_row_bytes(width: u32, bytes_per_pixel: u32, alignment: u32) -> u32 {
    align_up(width.saturating_mul(bytes_per_pixel), alignment)
}

/// Tightly packed RGBA pixels repacked into upload-ready, row-padded bytes.
///
/// Pixel rows are copied verbatim into a larger buffer whose stride is a
/// multiple of [`COPY_BYTES_PER_ROW_ALIGNMENT`]; the extra bytes at the end of
/// each row are zero. A hardware presenter can hand [`Self::bytes`] and
/// [`Self::bytes_per_row`] straight to `write_texture`, while the software path
/// can recover the original pixels with [`Self::tight_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureUpload {
    /// Width of the image in pixels.
    pub width: u32,
    /// Height of the image in pixels.
    pub height: u32,
    /// Padded stride of one row, in bytes.
    pub bytes_per_row: u32,
    /// Row-major pixel bytes with each row padded to `bytes_per_row`.
    pub bytes: Vec<u8>,
}

impl TextureUpload {
    /// Packs `rgba` (`width * height * 4` tight bytes) into padded rows.
    ///
    /// Returns [`GpuError::InvalidData`] when `rgba` does not match the stated
    /// dimensions.
    pub fn new(width: u32, height: u32, rgba: &[u8]) -> Result<Self, GpuError> {
        let tight_row = width as usize * 4;
        let expected = tight_row.saturating_mul(height as usize);
        if rgba.len() != expected {
            return Err(GpuError::InvalidData {
                expected,
                actual: rgba.len(),
            });
        }
        let bytes_per_row = padded_row_bytes(width, 4, COPY_BYTES_PER_ROW_ALIGNMENT);
        let mut bytes = vec![0_u8; bytes_per_row as usize * height as usize];
        for (src, dst) in rgba
            .chunks_exact(tight_row.max(1))
            .zip(bytes.chunks_exact_mut(bytes_per_row as usize))
        {
            dst[..tight_row].copy_from_slice(src);
        }
        Ok(Self {
            width,
            height,
            bytes_per_row,
            bytes,
        })
    }

    /// Tight (unpadded) stride of one row, in bytes.
    pub fn tight_row_bytes(&self) -> u32 {
        self.width * 4
    }

    /// Reconstructs tightly packed RGBA bytes by dropping the row padding.
    pub fn tight_bytes(&self) -> Vec<u8> {
        let tight_row = self.tight_row_bytes() as usize;
        let mut tight = Vec::with_capacity(tight_row * self.height as usize);
        if tight_row == 0 {
            return tight;
        }
        for row in self.bytes.chunks_exact(self.bytes_per_row as usize) {
            tight.extend_from_slice(&row[..tight_row]);
        }
        tight
    }

    /// The padded bytes of row `y`, including trailing padding.
    pub fn padded_row(&self, y: u32) -> Option<&[u8]> {
        let stride = self.bytes_per_row as usize;
        if stride == 0 || y as usize >= self.height as usize {
            return None;
        }
        self.bytes
            .get(y as usize * stride..y as usize * stride + stride)
    }

    /// The tight RGBA bytes of row `y`, without trailing padding.
    pub fn row(&self, y: u32) -> Option<&[u8]> {
        let tight_row = self.tight_row_bytes() as usize;
        self.padded_row(y).map(|row| &row[..tight_row])
    }

    /// True when the image contains no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// A pair of frame slots alternating between "being drawn" and "on screen".
///
/// Draw into [`Self::back_mut`], then call [`Self::swap`] to make that slot the
/// front buffer presented on the next frame; the old front buffer becomes the
/// new back buffer. Works for CPU [`ImageBuffer`]s as well as GPU texture
/// handles.
#[derive(Debug, Clone, PartialEq)]
pub struct DoubleBuffer<T> {
    slots: [T; 2],
    front: usize,
}

/// A double buffer of CPU RGBA frames.
pub type FrameDoubleBuffer = DoubleBuffer<ImageBuffer>;

impl<T> DoubleBuffer<T> {
    /// Creates a double buffer with `front` on screen and `back` ready to draw.
    pub fn new(front: T, back: T) -> Self {
        Self {
            slots: [front, back],
            front: 0,
        }
    }

    /// The slot currently being presented.
    pub fn front(&self) -> &T {
        &self.slots[self.front]
    }

    /// The slot that should be drawn into next.
    pub fn back(&self) -> &T {
        &self.slots[self.front ^ 1]
    }

    /// Mutable access to the slot that should be drawn into next.
    pub fn back_mut(&mut self) -> &mut T {
        let index = self.front ^ 1;
        &mut self.slots[index]
    }

    /// Swaps the slots so the back buffer becomes the front buffer.
    pub fn swap(&mut self) {
        self.front ^= 1;
    }

    /// Index (`0` or `1`) of the currently presented slot.
    pub fn index(&self) -> usize {
        self.front
    }

    /// Consumes the buffer and returns both slots as `[front, back]`.
    pub fn into_inner(self) -> [T; 2] {
        // The stored order is [slot0, slot1]; reorder so the front comes first.
        let [a, b] = self.slots;
        if self.front == 0 { [a, b] } else { [b, a] }
    }
}

impl DoubleBuffer<ImageBuffer> {
    /// Creates two blank RGBA frame buffers of the given size.
    pub fn new_frames(width: u32, height: u32) -> Self {
        Self::new(
            ImageBuffer::new(width, height),
            ImageBuffer::new(width, height),
        )
    }
}

/// An axis-aligned region of a texture, in pixels, with integer coordinates.
///
/// Used to describe which part of a texture changed so a presenter can copy
/// only those texels instead of the whole surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirtyRect {
    /// Left edge, inclusive.
    pub x: u32,
    /// Top edge, inclusive.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl DirtyRect {
    /// An empty rectangle at the origin.
    pub const EMPTY: Self = Self {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    /// Creates a rectangle from a top-left corner and a size.
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge, exclusive (saturating on overflow).
    pub fn right(&self) -> u32 {
        self.x.saturating_add(self.width)
    }

    /// Bottom edge, exclusive (saturating on overflow).
    pub fn bottom(&self) -> u32 {
        self.y.saturating_add(self.height)
    }

    /// True when the rectangle covers no pixels.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// True when the pixel at `(x, y)` lies inside the rectangle.
    pub fn contains(&self, x: u32, y: u32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// Smallest rectangle containing both `self` and `other`.
    ///
    /// Empty rectangles are treated as no-ops so a tracker can union safely.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    /// Overlap of the two rectangles, or an empty rectangle when they do not
    /// touch.
    pub fn intersection(self, other: Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Self::new(x, y, right.saturating_sub(x), bottom.saturating_sub(y))
    }

    /// Clamps the rectangle to a texture of `width` by `height` pixels.
    pub fn clamp(self, width: u32, height: u32) -> Self {
        self.intersection(Self::new(0, 0, width, height))
    }

    /// Number of tightly packed RGBA bytes this region occupies.
    pub fn byte_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Extracts this region from a full, tightly packed RGBA frame.
    ///
    /// `rgba` must hold at least `full_width * full_height * 4` bytes and the
    /// region must fit inside the frame. The returned [`SubImageUpload`] holds
    /// only the region's pixels, ready for a partial texture write.
    pub fn extract(
        &self,
        rgba: &[u8],
        full_width: u32,
        full_height: u32,
    ) -> Result<SubImageUpload, GpuError> {
        if full_width == 0
            || full_height == 0
            || self.right() > full_width
            || self.bottom() > full_height
        {
            return Err(GpuError::OutOfBounds {
                region: *self,
                texture: (full_width, full_height),
            });
        }
        let expected = full_width as usize * full_height as usize * 4;
        if rgba.len() < expected {
            return Err(GpuError::InvalidData {
                expected,
                actual: rgba.len(),
            });
        }
        let row_stride = full_width as usize * 4;
        let region_stride = self.width as usize * 4;
        let start = self.x as usize * 4;
        let mut bytes = Vec::with_capacity(self.byte_len());
        for row in rgba
            .chunks_exact(row_stride)
            .skip(self.y as usize)
            .take(self.height as usize)
        {
            bytes.extend_from_slice(&row[start..start + region_stride]);
        }
        Ok(SubImageUpload {
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            bytes,
        })
    }
}

/// Tightly packed pixels for one rectangular part of a texture.
///
/// Produced by [`DirtyRect::extract`] and consumed by
/// [`SoftwareDevice::upload_sub_image`]; a hardware presenter uses the origin
/// and size directly as the `origin` and `extent` of a `write_texture` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubImageUpload {
    /// Left edge of the region in the destination texture.
    pub x: u32,
    /// Top edge of the region in the destination texture.
    pub y: u32,
    /// Width of the region in pixels.
    pub width: u32,
    /// Height of the region in pixels.
    pub height: u32,
    /// Tightly packed RGBA bytes, `width * height * 4` in size.
    pub bytes: Vec<u8>,
}

impl SubImageUpload {
    /// Number of bytes expected in [`Self::bytes`].
    pub fn byte_len(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// True when the region covers no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The region as a [`DirtyRect`].
    pub fn rect(&self) -> DirtyRect {
        DirtyRect::new(self.x, self.y, self.width, self.height)
    }
}

/// Accumulates dirty rectangles into a single bounding box.
///
/// Mark every changed region during a frame, then call [`Self::take`] once to
/// get the bounding box to upload (or [`Self::bounds`] to peek without reset).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyTracker {
    bounds: Option<DirtyRect>,
}

impl DirtyTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a changed region. Empty rectangles are ignored.
    pub fn mark(&mut self, rect: DirtyRect) {
        if rect.is_empty() {
            return;
        }
        self.bounds = Some(match self.bounds {
            Some(bounds) => bounds.union(rect),
            None => rect,
        });
    }

    /// Adds several changed regions.
    pub fn mark_many(&mut self, rects: impl IntoIterator<Item = DirtyRect>) {
        for rect in rects {
            self.mark(rect);
        }
    }

    /// True when no non-empty region has been marked.
    pub fn is_empty(&self) -> bool {
        self.bounds.is_none()
    }

    /// Bounding box of everything marked so far, if any.
    pub fn bounds(&self) -> Option<DirtyRect> {
        self.bounds
    }

    /// Returns the bounding box and clears the tracker.
    pub fn take(&mut self) -> Option<DirtyRect> {
        self.bounds.take()
    }

    /// Clears the tracker without returning the bounds.
    pub fn clear(&mut self) {
        self.bounds = None;
    }
}

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

    /// Uploads a row-padded texture, using the default un-padding path.
    ///
    /// Backends that accept padded data directly may override this; the default
    /// simply removes the padding and delegates to [`Device::upload_texture`].
    fn upload_texture_padded(
        &mut self,
        id: TextureId,
        upload: &TextureUpload,
    ) -> Result<(), GpuError> {
        let tight = upload.tight_bytes();
        self.upload_texture(id, upload.width, upload.height, &tight)
    }
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

    /// Patches a sub-rectangle of an existing texture in place.
    ///
    /// Pixels outside `sub` are left untouched. This is the software path's
    /// dirty-rect upload: combined with [`DirtyRect::extract`] it transfers
    /// only the changed region instead of the whole texture.
    pub fn upload_sub_image(
        &mut self,
        id: TextureId,
        sub: &SubImageUpload,
    ) -> Result<(), GpuError> {
        let Some(texture) = self.textures.get_mut(id.0) else {
            return Err(GpuError::UnknownTexture(id));
        };
        if sub.is_empty() {
            return Ok(());
        }
        let region = sub.rect();
        if region.right() > texture.0 || region.bottom() > texture.1 {
            return Err(GpuError::OutOfBounds {
                region,
                texture: (texture.0, texture.1),
            });
        }
        if sub.bytes.len() < sub.byte_len() {
            return Err(GpuError::InvalidData {
                expected: sub.byte_len(),
                actual: sub.bytes.len(),
            });
        }
        let row_bytes = sub.width as usize * 4;
        let dst_stride = texture.0 as usize * 4;
        let start = sub.x as usize * 4;
        for (dst_row, src_row) in texture
            .2
            .chunks_exact_mut(dst_stride)
            .skip(sub.y as usize)
            .take(sub.height as usize)
            .zip(sub.bytes.chunks_exact(row_bytes))
        {
            dst_row[start..start + row_bytes].copy_from_slice(src_row);
        }
        Ok(())
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
        assert!(LAYER_SHADER.contains("fn blend_layer"));
        assert_eq!(RECT_SHADER, include_str!("shaders/rect.wgsl"));
        assert_eq!(LAYER_SHADER, include_str!("shaders/layer.wgsl"));
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

    #[test]
    fn alignment_helpers_round_rows_up() {
        assert_eq!(align_up(0, 4), 0);
        assert_eq!(align_up(1, 4), 4);
        assert_eq!(align_up(5, 4), 8);
        assert_eq!(align_up(8, 4), 8);
        assert_eq!(align_up(3, 1), 3);
        assert_eq!(
            padded_row_bytes(3, 4, COPY_BYTES_PER_ROW_ALIGNMENT),
            COPY_BYTES_PER_ROW_ALIGNMENT
        );
        assert_eq!(padded_row_bytes(64, 4, 256), 256);
        assert_eq!(padded_row_bytes(65, 4, 256), 512);
    }

    #[test]
    fn texture_upload_pads_and_restores_rows() {
        // 3 RGBA pixels per row = 12 tight bytes, padded out to 256.
        let tight: Vec<u8> = (0..24).map(|value| value as u8).collect();
        let upload = TextureUpload::new(3, 2, &tight).unwrap();
        assert_eq!(upload.tight_row_bytes(), 12);
        assert_eq!(upload.bytes_per_row, 256);
        assert_eq!(upload.bytes.len(), 512);
        assert_eq!(upload.row(0), Some(&tight[0..12]));
        assert_eq!(upload.row(1), Some(&tight[12..24]));
        assert_eq!(upload.padded_row(0).unwrap().len(), 256);
        assert!(upload.padded_row(0).unwrap()[12..].iter().all(|&b| b == 0));
        assert_eq!(upload.padded_row(2), None);
        assert_eq!(upload.tight_bytes(), tight);
        assert!(!upload.is_empty());

        assert_eq!(
            TextureUpload::new(2, 2, &[0; 7]),
            Err(GpuError::InvalidData {
                expected: 16,
                actual: 7
            })
        );
    }

    #[test]
    fn padded_upload_delegates_to_tight_texture_write() {
        let tight: Vec<u8> = (0..16).map(|value| 100 + value as u8).collect();
        let upload = TextureUpload::new(2, 2, &tight).unwrap();
        let mut device = SoftwareDevice::default();
        let id = device.create_texture(2, 2);
        device.upload_texture_padded(id, &upload).unwrap();
        assert_eq!(device.read_texture(id).unwrap(), tight.as_slice());
    }

    #[test]
    fn double_buffer_alternates_front_and_back() {
        let mut buffer = DoubleBuffer::new(0_u8, 1_u8);
        assert_eq!(*buffer.front(), 0);
        assert_eq!(*buffer.back(), 1);
        assert_eq!(buffer.index(), 0);

        *buffer.back_mut() = 2;
        assert_eq!(*buffer.back(), 2);
        buffer.swap();
        assert_eq!(*buffer.front(), 2);
        assert_eq!(*buffer.back(), 0);
        assert_eq!(buffer.index(), 1);

        buffer.swap();
        assert_eq!(*buffer.front(), 0);
        assert_eq!(*buffer.back(), 2);
        assert_eq!(buffer.into_inner(), [0, 2]);
    }

    #[test]
    fn frame_double_buffer_creates_blank_frames() {
        let frames: FrameDoubleBuffer = DoubleBuffer::new_frames(4, 3);
        assert_eq!(frames.front().width, 4);
        assert_eq!(frames.front().height, 3);
        assert_eq!(frames.back().pixels.len(), 4 * 3 * 4);
        assert!(frames.front().pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn dirty_rect_geometry() {
        let a = DirtyRect::new(4, 6, 3, 5);
        let b = DirtyRect::new(2, 9, 4, 2);
        assert_eq!(a.right(), 7);
        assert_eq!(a.bottom(), 11);
        assert!(a.contains(4, 6));
        assert!(a.contains(6, 10));
        assert!(!a.contains(7, 6));
        assert!(DirtyRect::EMPTY.is_empty());
        assert_eq!(a.union(b), DirtyRect::new(2, 6, 5, 5));
        assert_eq!(a.intersection(b), DirtyRect::new(4, 9, 2, 2));
        assert_eq!(
            DirtyRect::new(1, 1, 2, 2).clamp(2, 2),
            DirtyRect::new(1, 1, 1, 1)
        );
        assert_eq!(a.union(DirtyRect::EMPTY), a);
        assert_eq!(a.byte_len(), 3 * 5 * 4);
    }

    #[test]
    fn dirty_tracker_accumulates_bounding_box_and_resets() {
        let mut tracker = DirtyTracker::new();
        assert!(tracker.is_empty());
        tracker.mark(DirtyRect::new(2, 3, 4, 4));
        tracker.mark_many([
            DirtyRect::new(8, 1, 2, 2),
            DirtyRect::EMPTY,
            DirtyRect::new(0, 9, 3, 1),
        ]);
        assert_eq!(tracker.bounds(), Some(DirtyRect::new(0, 1, 10, 9)));
        assert_eq!(tracker.take(), Some(DirtyRect::new(0, 1, 10, 9)));
        assert!(tracker.is_empty());
        tracker.mark(DirtyRect::new(1, 1, 1, 1));
        tracker.clear();
        assert!(tracker.is_empty());
    }

    #[test]
    fn dirty_region_upload_patches_only_changed_pixels() {
        let width = 4;
        let height = 4;
        let frame: Vec<u8> = (0..width * height * 4).map(|value| value as u8).collect();

        let mut device = SoftwareDevice::default();
        let id = device.create_texture(width, height);
        device.upload_texture(id, width, height, &frame).unwrap();
        let before = device.read_texture(id).unwrap().to_vec();

        let sub = DirtyRect::new(1, 2, 2, 1)
            .extract(&frame, width, height)
            .unwrap();
        assert_eq!(sub.bytes.len(), 8);
        assert_eq!(sub.rect(), DirtyRect::new(1, 2, 2, 1));
        let patch = [200_u8, 201, 202, 203, 204, 205, 206, 207];
        let sub = SubImageUpload {
            bytes: patch.to_vec(),
            ..sub
        };
        device.upload_sub_image(id, &sub).unwrap();

        let after = device.read_texture(id).unwrap();
        assert_ne!(before, after);
        // The patched row is at y=2, columns 1..3 (bytes 36..44).
        assert_eq!(&after[36..44], &patch);
        // Everything outside the region is unchanged.
        for (index, (old, new)) in before.iter().zip(after.iter()).enumerate() {
            if !(36..44).contains(&index) {
                assert_eq!(
                    old, new,
                    "pixel byte {index} changed outside the dirty rect"
                );
            }
        }
    }

    #[test]
    fn dirty_region_upload_rejects_invalid_input() {
        let mut device = SoftwareDevice::default();
        let id = device.create_texture(2, 2);
        let frame = [0_u8; 16];

        assert!(matches!(
            DirtyRect::new(0, 0, 3, 1).extract(&frame, 2, 2),
            Err(GpuError::OutOfBounds { .. })
        ));
        assert!(matches!(
            DirtyRect::new(0, 0, 1, 1).extract(&frame[..15], 2, 2),
            Err(GpuError::InvalidData { .. })
        ));

        let sub = SubImageUpload {
            x: 2,
            y: 0,
            width: 1,
            height: 1,
            bytes: [1, 2, 3, 4].to_vec(),
        };
        assert_eq!(
            device.upload_sub_image(id, &sub),
            Err(GpuError::OutOfBounds {
                region: DirtyRect::new(2, 0, 1, 1),
                texture: (2, 2),
            })
        );
        let short = SubImageUpload {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            bytes: vec![1, 2],
        };
        assert_eq!(
            device.upload_sub_image(id, &short),
            Err(GpuError::InvalidData {
                expected: 4,
                actual: 2
            })
        );
        assert_eq!(
            device.upload_sub_image(TextureId(42), &short),
            Err(GpuError::UnknownTexture(TextureId(42)))
        );
    }
}

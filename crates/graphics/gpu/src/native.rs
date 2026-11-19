//! Native `wgpu` device and multi-layer surface presenter.
//!
//! Page and chrome are still rasterized deterministically on the CPU (the
//! bitmap font must stay byte-identical with headless screenshots). Instead of
//! uploading one full-screen image, [`NativePresenter::present_layers`] uploads
//! each compositor layer into its own slice of a texture array and composites
//! them back-to-front in one draw with [`crate::device::LAYER_SHADER`]. The WGSL
//! shader applies per-layer opacity, an inverse transform matrix and one of the
//! supported blend modes. [`LayerTextureCache`] skips unchanged layers and
//! [`crate::device::DirtyRect`] limits transfers to changed pixels.
//!
//! The legacy [`NativePresenter::present`] entry point remains as a fallback:
//! it wraps a single [`ImageBuffer`] in a fully opaque `LayerFrame` and calls
//! the same compositor. The module is feature gated (`wgpu-backend`) so the
//! default workspace build never pulls in `wgpu`; SoftwareDevice is untouched.

use browser_paint::ImageBuffer;

use crate::device::{BlendMode, DirtyRect, GpuError, LAYER_SHADER};
use crate::layer::{
    DEFAULT_LAYER_CAPACITY, LayerFrame, LayerTextureCache, LayerTransform, UploadAction,
    padded_upload,
};

/// Bytes occupied by one `LayerParams` entry in the shader storage buffer.
const LAYER_PARAMS_BYTES: usize = 80;

/// Unit quad: position (0..1) and UV (0..1). The vertex shader scales it by the
/// viewport, so resizing never has to rebuild this buffer.
const QUAD_VERTICES: [[f32; 4]; 6] = [
    [0.0, 0.0, 0.0, 0.0],
    [1.0, 0.0, 1.0, 0.0],
    [0.0, 1.0, 0.0, 1.0],
    [1.0, 0.0, 1.0, 0.0],
    [1.0, 1.0, 1.0, 1.0],
    [0.0, 1.0, 0.0, 1.0],
];

/// Errors reported by the native presentation path.
#[derive(Debug)]
pub enum NativeError {
    /// No suitable graphics adapter was found.
    NoAdapter(String),
    /// The logical device could not be created.
    Device(String),
    /// The OS window could not be turned into a wgpu surface.
    Surface(String),
    /// The surface does not expose an sRGB format.
    SurfaceFormat(String),
    /// Acquiring or presenting a frame failed.
    Present(String),
    /// A layer texture could not be uploaded.
    Upload(GpuError),
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(message) => write!(f, "no graphics adapter: {message}"),
            Self::Device(message) => write!(f, "could not create graphics device: {message}"),
            Self::Surface(message) => write!(f, "could not create window surface: {message}"),
            Self::SurfaceFormat(message) => write!(f, "unsupported surface format: {message}"),
            Self::Present(message) => write!(f, "presentation failed: {message}"),
            Self::Upload(error) => write!(f, "layer upload failed: {error}"),
        }
    }
}

impl std::error::Error for NativeError {}

/// A native GPU device plus the resources shared by every surface.
pub struct NativeDevice {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
}

impl NativeDevice {
    /// Requests an adapter and a device.
    pub fn new() -> Result<Self, NativeError> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
        }))
        .map_err(|error| NativeError::NoAdapter(error.to_string()))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mag-device"),
            ..Default::default()
        }))
        .map_err(|error| NativeError::Device(error.to_string()))?;
        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            info,
        })
    }

    /// Adapter name, useful for the status line and logs.
    pub fn adapter_name(&self) -> &str {
        &self.info.name
    }

    /// Backend name (Metal, Vulkan, …).
    pub fn backend_name(&self) -> &'static str {
        self.info.backend.to_str()
    }

    /// Creates a surface and the presentation pipeline for a window handle.
    pub fn create_presenter<T>(
        &self,
        target: T,
        width: u32,
        height: u32,
    ) -> Result<NativePresenter, NativeError>
    where
        T: Into<wgpu::SurfaceTarget<'static>>,
    {
        let surface = self
            .instance
            .create_surface(target)
            .map_err(|error| NativeError::Surface(error.to_string()))?;
        NativePresenter::new(
            &self.adapter,
            self.device.clone(),
            self.queue.clone(),
            surface,
            width,
            height,
        )
    }
}

/// Owns one window surface and composites layer textures onto it.
pub struct NativePresenter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    frame_layout: wgpu::BindGroupLayout,
    frame_bind_group: wgpu::BindGroup,
    globals: wgpu::Buffer,
    layer_params: wgpu::Buffer,
    layer_params_capacity: usize,
    texture_layout: wgpu::BindGroupLayout,
    texture_bind_group: wgpu::BindGroup,
    layer_texture: wgpu::Texture,
    texture_width: u32,
    texture_height: u32,
    texture_depth: u32,
    vertex_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    cache: LayerTextureCache,
    format: wgpu::TextureFormat,
}

impl NativePresenter {
    fn new(
        adapter: &wgpu::Adapter,
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        width: u32,
        height: u32,
    ) -> Result<Self, NativeError> {
        let caps = surface.get_capabilities(adapter);
        if caps.formats.is_empty() {
            return Err(NativeError::SurfaceFormat(
                "the window surface is incompatible with this adapter".to_string(),
            ));
        }
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .ok_or_else(|| {
                NativeError::SurfaceFormat(
                    "no sRGB surface format is available for this window".to_string(),
                )
            })?;
        let width = width.max(1);
        let height = height.max(1);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width,
            height,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mag-layer-shader"),
            source: wgpu::ShaderSource::Wgsl(LAYER_SHADER.into()),
        });

        // Group 0: viewport globals (uniform) and per-layer shader parameters
        // (read-only storage array).
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mag-layer-frame-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        // Group 1: the layer texture array plus its sampler.
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mag-layer-texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mag-layer-pipeline-layout"),
            bind_group_layouts: &[Some(&frame_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mag-layer-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 16,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-layer-globals"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layer_params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-layer-params"),
            size: (DEFAULT_LAYER_CAPACITY * LAYER_PARAMS_BYTES) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mag-layer-frame-bind"),
            layout: &frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: layer_params.as_entire_binding(),
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mag-layer-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let (layer_texture, texture_bind_group) = Self::layer_texture(
            &device,
            &texture_layout,
            &sampler,
            width,
            height,
            DEFAULT_LAYER_CAPACITY as u32,
        );

        let mut vertex_bytes = Vec::with_capacity(QUAD_VERTICES.len() * 16);
        for vertex in QUAD_VERTICES {
            for value in vertex {
                vertex_bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-layer-quad"),
            size: vertex_bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&vertex_buffer, 0, &vertex_bytes);

        Ok(Self {
            device,
            queue,
            surface,
            config,
            pipeline,
            frame_layout,
            frame_bind_group,
            globals,
            layer_params,
            layer_params_capacity: DEFAULT_LAYER_CAPACITY,
            texture_layout,
            texture_bind_group,
            layer_texture,
            texture_width: width,
            texture_height: height,
            texture_depth: DEFAULT_LAYER_CAPACITY as u32,
            vertex_buffer,
            sampler,
            cache: LayerTextureCache::new(DEFAULT_LAYER_CAPACITY),
            format,
        })
    }

    fn layer_texture(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
        layers: u32,
    ) -> (wgpu::Texture, wgpu::BindGroup) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mag-layer-textures"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: layers,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("mag-layer-textures-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mag-layer-textures-bind"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        (texture, bind_group)
    }

    fn write_globals(&self, width: u32, height: u32, layer_count: usize) {
        let floats = [width as f32, height as f32, layer_count as f32, 0.0];
        let mut bytes = [0_u8; 16];
        for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(floats) {
            chunk.copy_from_slice(&value.to_ne_bytes());
        }
        self.queue.write_buffer(&self.globals, 0, &bytes);
    }

    fn ensure_layer_params(&mut self, layer_count: usize) {
        if layer_count <= self.layer_params_capacity {
            return;
        }
        let capacity = layer_count.next_power_of_two();
        self.layer_params = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-layer-params"),
            size: (capacity * LAYER_PARAMS_BYTES) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.frame_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mag-layer-frame-bind"),
            layout: &self.frame_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.globals.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.layer_params.as_entire_binding(),
                },
            ],
        });
        self.layer_params_capacity = capacity;
    }

    fn write_layer_params(&self, frames: &[LayerFrame<'_>]) {
        let mut floats = vec![0.0_f32; frames.len() * (LAYER_PARAMS_BYTES / 4)];
        for (index, frame) in frames.iter().enumerate() {
            let base = index * (LAYER_PARAMS_BYTES / 4);
            let inverse = frame
                .transform
                .inverse()
                .unwrap_or(LayerTransform::IDENTITY);
            floats[base..base + 16].copy_from_slice(&inverse.as_array());
            floats[base + 16] = frame.width as f32;
            floats[base + 17] = frame.height as f32;
            floats[base + 18] = frame.opacity.clamp(0.0, 1.0);
            floats[base + 19] = f32::from_bits(frame.blend.shader_index());
        }
        let bytes: Vec<u8> = floats
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect();
        self.queue.write_buffer(&self.layer_params, 0, &bytes);
    }

    fn ensure_layer_texture(&mut self, frames: &[LayerFrame<'_>]) {
        let required_width = frames
            .iter()
            .map(|frame| frame.width)
            .max()
            .unwrap_or(0)
            .max(self.config.width);
        let required_height = frames
            .iter()
            .map(|frame| frame.height)
            .max()
            .unwrap_or(0)
            .max(self.config.height);
        let depth = self.cache.capacity() as u32;
        if self.texture_width >= required_width
            && self.texture_height >= required_height
            && self.texture_depth >= depth
        {
            return;
        }
        let (texture, bind_group) = Self::layer_texture(
            &self.device,
            &self.texture_layout,
            &self.sampler,
            required_width,
            required_height,
            depth,
        );
        self.layer_texture = texture;
        self.texture_bind_group = bind_group;
        self.texture_width = required_width;
        self.texture_height = required_height;
        self.texture_depth = depth;
        // Different-sized slices hold completely new content: force full uploads.
        self.cache.clear();
    }

    fn upload_layers(&mut self, frames: &[LayerFrame<'_>]) -> Result<(), NativeError> {
        for frame in frames {
            let action = self.cache.plan(frame).map_err(NativeError::Upload)?;
            let UploadAction::Upload { slot, sub } = action else {
                continue;
            };
            let padded = padded_upload(&sub).map_err(NativeError::Upload)?;
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.layer_texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: sub.x,
                        y: sub.y,
                        z: slot as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &padded.bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded.bytes_per_row),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width: sub.width,
                    height: sub.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        Ok(())
    }

    fn acquire_surface(&mut self) -> Result<Option<wgpu::SurfaceTexture>, NativeError> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Ok(Some(texture)),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(texture)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => Ok(Some(texture)),
                    other => Err(NativeError::Present(format!(
                        "swap chain is still unavailable: {}",
                        surface_status(&other)
                    ))),
                }
            }
            other => Err(NativeError::Present(surface_status(&other))),
        }
    }

    /// Composites the supplied layers back-to-front and presents the result.
    ///
    /// Returns `Ok(false)` when the frame was skipped (timeout or occluded
    /// window), which is not an error. Layers are drawn in slice order; the
    /// caller is responsible for sorting them by stacking order.
    pub fn present_layers(&mut self, frames: &[LayerFrame<'_>]) -> Result<bool, NativeError> {
        let frames: Vec<LayerFrame<'_>> = frames
            .iter()
            .filter(|frame| !frame.is_empty())
            .cloned()
            .collect();
        if frames.is_empty() {
            return Ok(false);
        }
        self.ensure_layer_texture(&frames);
        self.ensure_layer_params(frames.len());
        self.write_globals(self.config.width, self.config.height, frames.len());
        self.write_layer_params(&frames);
        self.upload_layers(&frames)?;

        let Some(surface_texture) = self.acquire_surface()? else {
            return Ok(false);
        };
        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mag-layer-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mag-layer-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.frame_bind_group, &[]);
            pass.set_bind_group(1, &self.texture_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(surface_texture);
        Ok(true)
    }

    /// Uploads a completed window image and presents it as a single layer.
    ///
    /// Retained as the fallback for callers that only have one rasterized image.
    pub fn present(&mut self, image: &ImageBuffer) -> Result<bool, NativeError> {
        if image.width == 0 || image.height == 0 {
            return Ok(false);
        }
        let frame = LayerFrame::new(
            0,
            image,
            browser_layout::Rect::new(
                0.0,
                0.0,
                self.config.width as f32,
                self.config.height as f32,
            ),
        )
        .with_opacity(1.0)
        .with_blend(BlendMode::Normal)
        .with_dirty(Some(DirtyRect::new(0, 0, image.width, image.height)));
        self.present_layers(&[frame])
    }

    /// Surface colour format chosen for this window.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Number of layers currently cached.
    pub fn cached_layers(&self) -> usize {
        self.cache.len()
    }

    /// Forgets cached layer content (for example after a resize).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Reconfigures the swap chain for a new window size.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if self.config.width == width && self.config.height == height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.clear_cache();
    }
}

fn surface_status(texture: &wgpu::CurrentSurfaceTexture) -> String {
    match texture {
        wgpu::CurrentSurfaceTexture::Success(_) => "success".to_string(),
        wgpu::CurrentSurfaceTexture::Suboptimal(_) => "suboptimal".to_string(),
        wgpu::CurrentSurfaceTexture::Timeout => "timeout".to_string(),
        wgpu::CurrentSurfaceTexture::Occluded => "window occluded".to_string(),
        wgpu::CurrentSurfaceTexture::Outdated => "surface outdated".to_string(),
        wgpu::CurrentSurfaceTexture::Lost => "surface lost".to_string(),
        wgpu::CurrentSurfaceTexture::Validation => "validation error".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_with_context() {
        let error = NativeError::SurfaceFormat("no sRGB".to_string());
        assert!(error.to_string().contains("no sRGB"));
        assert!(
            NativeError::NoAdapter("none".into())
                .to_string()
                .contains("adapter")
        );
        assert!(
            NativeError::Upload(GpuError::NotConfigured)
                .to_string()
                .contains("layer upload")
        );
    }

    #[test]
    fn layer_shader_parses_without_an_adapter() {
        let module = wgpu::naga::front::wgsl::parse_str(LAYER_SHADER)
            .expect("the compositor shader must be valid WGSL");
        assert!(module.entry_points.iter().any(|ep| ep.name == "fs_main"));
        assert!(module.entry_points.iter().any(|ep| ep.name == "vs_main"));
    }

    #[test]
    fn layer_params_fit_the_declared_stride() {
        // Sixteen matrix floats, two size floats, opacity and blend index.
        assert_eq!(LAYER_PARAMS_BYTES, 20 * std::mem::size_of::<f32>());
        let mut floats = [0.0_f32; 20];
        floats[19] = f32::from_bits(BlendMode::Overlay.shader_index());
        assert_eq!(u32::from_le_bytes(floats[19].to_le_bytes()), 3);
    }
}

//! Native `wgpu` device and surface presenter.
//!
//! The engine's page and chrome are rasterized deterministically on the CPU
//! (the bitmap font must stay byte-identical with headless screenshots), then
//! uploaded once per frame as an sRGB texture and drawn over the whole surface
//! by a single textured-quad pipeline. That keeps the GPU path small: one
//! shader, one texture, one draw call, while still using the native swap chain
//! and vsync.
//!
//! The module is feature gated (`wgpu-backend`) so the default workspace build
//! never pulls in `wgpu`. All fallible platform steps (adapter request, surface
//! creation, swap-chain acquisition) return [`NativeError`] instead of panicking.

use browser_paint::ImageBuffer;

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
}

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter(message) => write!(f, "no graphics adapter: {message}"),
            Self::Device(message) => write!(f, "could not create graphics device: {message}"),
            Self::Surface(message) => write!(f, "could not create window surface: {message}"),
            Self::SurfaceFormat(message) => write!(f, "unsupported surface format: {message}"),
            Self::Present(message) => write!(f, "presentation failed: {message}"),
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

struct FrameTexture {
    width: u32,
    height: u32,
    _texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// Owns one window surface and presents RGBA frames onto it.
pub struct NativePresenter {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    uniform: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    sampler: wgpu::Sampler,
    frame: Option<FrameTexture>,
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
            label: Some("mag-present-shader"),
            source: wgpu::ShaderSource::Wgsl(super::IMAGE_SHADER.into()),
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mag-present-uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mag-present-texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mag-present-layout"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mag-present-pipeline"),
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

        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-present-viewport"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let vertex_buffer = Self::vertex_buffer(&device, &queue, width, height);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("mag-present-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mag-present-uniform-bind"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let presenter = Self {
            device,
            queue,
            surface,
            config,
            pipeline,
            texture_layout,
            uniform,
            uniform_bind_group,
            vertex_buffer,
            sampler,
            frame: None,
            format,
        };
        presenter.write_viewport(width, height);
        Ok(presenter)
    }

    fn vertex_buffer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        width: u32,
        height: u32,
    ) -> wgpu::Buffer {
        let (w, h) = (width as f32, height as f32);
        let vertices: [[f32; 4]; 6] = [
            [0.0, 0.0, 0.0, 0.0],
            [w, 0.0, 1.0, 0.0],
            [0.0, h, 0.0, 1.0],
            [w, 0.0, 1.0, 0.0],
            [w, h, 1.0, 1.0],
            [0.0, h, 0.0, 1.0],
        ];
        let mut bytes = Vec::with_capacity(vertices.len() * 16);
        for vertex in vertices {
            for value in vertex {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mag-present-quad"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, &bytes);
        buffer
    }

    fn write_viewport(&self, width: u32, height: u32) {
        let mut bytes = [0_u8; 16];
        bytes[8..12].copy_from_slice(&(width as f32).to_ne_bytes());
        bytes[12..16].copy_from_slice(&(height as f32).to_ne_bytes());
        self.queue.write_buffer(&self.uniform, 0, &bytes);
    }

    /// Surface colour format chosen for this window.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
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
        self.vertex_buffer = Self::vertex_buffer(&self.device, &self.queue, width, height);
    }

    fn ensure_frame(&mut self, width: u32, height: u32) {
        if let Some(frame) = &self.frame
            && frame.width == width
            && frame.height == height
        {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("mag-present-frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mag-present-frame-bind"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.frame = Some(FrameTexture {
            width,
            height,
            _texture: texture,
            bind_group,
        });
    }

    /// Uploads a completed window image and presents it.
    ///
    /// Returns `Ok(false)` when the frame was skipped (timeout or occluded
    /// window), which is not an error.
    pub fn present(&mut self, image: &ImageBuffer) -> Result<bool, NativeError> {
        if image.width == 0 || image.height == 0 {
            return Ok(false);
        }
        self.ensure_frame(image.width, image.height);
        let Some(frame) = &self.frame else {
            return Ok(false);
        };
        self.write_viewport(image.width, image.height);
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &frame._texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &image.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(image.width * 4),
                rows_per_image: Some(image.height),
            },
            wgpu::Extent3d {
                width: image.width,
                height: image.height,
                depth_or_array_layers: 1,
            },
        );

        let surface_texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(texture)
            | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(texture)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(texture) => texture,
                    other => {
                        return Err(NativeError::Present(format!(
                            "swap chain is still unavailable: {}",
                            surface_status(&other)
                        )));
                    }
                }
            }
            other => {
                return Err(NativeError::Present(surface_status(&other)));
            }
        };

        let view = surface_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mag-present-encoder"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mag-present-pass"),
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
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &frame.bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
            pass.draw(0..6, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(surface_texture);
        Ok(true)
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
    }
}

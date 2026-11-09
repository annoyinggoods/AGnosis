//! The wgpu back end: the browser's WebGPU and its WebGL2.
//!
//! Both are the same `wgpu` code path — only the **instance's backends**,
//! the **order** of the adapter and the surface, and the **limits** differ:
//!
//! | back end | instance backends | order | limits |
//! |---|---|---|---|
//! | WebGPU | `Backends::BROWSER_WEBGPU` | adapter, device, **then** surface | the adapter's own |
//! | WebGL2 | `Backends::GL` (the browser's `wgpu-hal` `gles/web.rs`) | **surface first**, then adapter | the adapter's own |
//!
//! The order is not cosmetic:
//!
//! * the **surface** is what takes the canvas' context, so the WebGPU
//!   path asks for its adapter and its device first — a browser without
//!   WebGPU then leaves the canvas untouched and the fall-back chain can
//!   carry on;
//! * the **WebGL2 backend enumerates its adapters *from* a surface**
//!   (`wgpu-hal`'s web GLES `enumerate_adapters` returns nothing without
//!   one), so `compatible_surface` is mandatory there and the surface
//!   has to come first.
//!
//! The WGSL and the pipeline are the **core's**
//! ([`agnosis_core::render::BLIT_SHADER`]): one fullscreen textured quad,
//! so the web build draws the same picture as the desktop one — and the
//! two cannot drift, because there is only one copy (audit 2026-10-21
//! §E9).

use web_sys::HtmlCanvasElement;

use super::{frame_is_valid, label, warn, Attempt, Backend, Renderer};

/// The wgpu back end: a surface, an upload texture and the quad pipeline.
pub(crate) struct Wgpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    layout: wgpu::BindGroupLayout,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    width: u32,
    height: u32,
    name: &'static str,
    /// The oversized-frame warning has already been printed.
    ///
    /// WHY: the warning was printed **per frame**, so a 4K canvas on a
    /// driver with a 2048 limit flooded the console (audit 2026-10-21
    /// §W4). Once is enough: the situation does not change by itself.
    warned_oversized: bool,
}

/// Build the wgpu back end for `kind` on `canvas`.
pub(crate) async fn create(canvas: &HtmlCanvasElement, kind: Backend) -> Result<Wgpu, Attempt> {
    match kind {
        Backend::WebGpu => create_webgpu(canvas).await,
        Backend::WebGl2 => create_webgl2(canvas).await,
        _ => Err(Attempt::failed("not a wgpu back end")),
    }
}

/// The browser's WebGPU.
async fn create_webgpu(canvas: &HtmlCanvasElement) -> Result<Wgpu, Attempt> {
    // `navigator.gpu` exists on browsers that cannot actually produce an
    // adapter — headless Chrome without a GPU flag is one — and
    // `Instance::new` only probes for the *property*. The real probe is
    // `is_browser_webgpu_supported`, which requests an adapter; and
    // `new_instance_with_webgpu_detection` drops the WebGPU back end from
    // the instance when that probe says no.
    if !wgpu::util::is_browser_webgpu_supported().await {
        return Err(Attempt::failed("the browser has no WebGPU adapter"));
    }
    let instance = wgpu::util::new_instance_with_webgpu_detection(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::BROWSER_WEBGPU,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    })
    .await;
    // The adapter and the device come **before** the surface: creating
    // the surface is what takes the canvas' `webgpu` context, and a
    // canvas that carries a context cannot serve another back end.
    let adapter = request_adapter(&instance, None).await?;
    let (device, queue) = request_device(&adapter).await?;
    let surface = create_surface(&instance, canvas)?;
    build(canvas, device, queue, surface, &adapter, Backend::WebGpu)
}

/// The browser's WebGL2, through `wgpu`'s browser GLES backend.
async fn create_webgl2(canvas: &HtmlCanvasElement) -> Result<Wgpu, Attempt> {
    // No `BROWSER_WEBGPU` in the backends: the instance must use
    // `wgpu-core`, which is where the browser GLES backend lives.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::GL,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    // The surface first: the WebGL2 backend enumerates its adapters
    // *from* a surface, so `compatible_surface` is mandatory — and the
    // surface is what takes the canvas' `webgl2` context.
    let surface = create_surface(&instance, canvas)?;
    let adapter = request_adapter(&instance, Some(&surface)).await?;
    let (device, queue) = request_device(&adapter).await?;
    build(canvas, device, queue, surface, &adapter, Backend::WebGl2)
}

/// Ask the instance for an adapter.
///
/// `surface` is `None` for WebGPU (the browser's WebGPU does not need
/// one) and `Some` for WebGL2 (**mandatory** there).
async fn request_adapter(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
) -> Result<wgpu::Adapter, Attempt> {
    let options = wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: surface,
        apply_limit_buckets: false,
    };
    instance
        .request_adapter(&options)
        .await
        .map_err(|error| Attempt::failed(format!("no adapter: {error}")))
}

/// Ask the adapter for a device.
///
/// The **adapter's own limits** are requested: they are the ones the
/// device can really give, and they are high enough for a 4K canvas
/// (the WebGL2 backend reports the driver's real `MAX_TEXTURE_SIZE`).
/// The WebGPU default limits would be lower than that, and the downlevel
/// WebGL2 ones (`max_texture_dimension_2d: 2048`) would not fit a 4K
/// frame at all.
async fn request_device(adapter: &wgpu::Adapter) -> Result<(wgpu::Device, wgpu::Queue), Attempt> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("agnosis_web"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        })
        .await
        .map_err(|error| Attempt::failed(format!("no device: {error}")))
}

/// Create the surface for `canvas`.
///
/// The surface is what takes the canvas' context — `webgpu` for the
/// WebGPU back end, `webgl2` for the WebGL2 one. A failure here means
/// the browser would not give the canvas a context; the canvas may or may
/// not be spent, so the chain carries on and the next back end reports
/// the truth.
fn create_surface(
    instance: &wgpu::Instance,
    canvas: &HtmlCanvasElement,
) -> Result<wgpu::Surface<'static>, Attempt> {
    instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|error| {
            Attempt::failed(format!(
                "the canvas gave no context: {error} — another back end may already have taken it"
            ))
        })
}

/// Build the renderer once the device, the queue and the surface are in hand.
fn build(
    canvas: &HtmlCanvasElement,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    adapter: &wgpu::Adapter,
    kind: Backend,
) -> Result<Wgpu, Attempt> {
    let width = canvas.width().max(1);
    let height = canvas.height().max(1);
    // From here on the canvas is spent: the surface exists, so it has a
    // context.
    let config = surface_config(&surface, adapter, width, height)?;
    surface.configure(&device, &config);
    let layout = bind_group_layout(&device);
    let (texture, texture_view) = upload_texture(&device, width, height, config.format);
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("ducks-web-sampler"),
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let bind_group = make_bind_group(&device, &layout, &texture_view, &sampler);
    let pipeline = make_pipeline(&device, &layout, config.format);
    Ok(Wgpu {
        device,
        queue,
        surface,
        config,
        layout,
        texture,
        texture_view,
        sampler,
        bind_group,
        pipeline,
        width,
        height,
        name: label(kind),
        warned_oversized: false,
    })
}

/// The surface configuration for `width` × `height`.
fn surface_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
) -> Result<wgpu::SurfaceConfiguration, Attempt> {
    let caps = surface.get_capabilities(adapter);
    let Some(format) = caps.formats.first().copied() else {
        return Err(Attempt::spent("the surface reports no formats"));
    };
    Ok(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: width.max(1),
        height: height.max(1),
        // The browser back ends **panic** on `Mailbox` and `Immediate`.
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        // The browser back ends **panic** on `PostMultiplied` and
        // `Inherit`; both of them report `Opaque` as their only mode.
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
    })
}

/// The RGBA8 texture the frame is uploaded into.
///
/// WHY the format follows the **surface**: the texture is *sampled* by
/// the shader, so an sRGB texture is decoded to linear on the way in,
/// and an sRGB surface encodes it back to sRGB on the way out. The
/// two cancel and the canvas carries the frame's own bytes.
///
/// A **linear** surface (the browser's WebGPU canvas has no sRGB
/// format at all) does neither, so it wants the bytes as they are —
/// a plain `Rgba8Unorm` upload texture. The native renderer has the
/// same rule (`src/render.rs`).
fn upload_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ducks-web-frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: super::upload_format(surface_format),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// The bind group layout: the frame texture and its sampler.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("ducks-web-bind-layout"),
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
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

/// The bind group for one upload-texture view and its sampler.
fn make_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("ducks-web-bind"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// The fullscreen-quad pipeline.
fn make_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("ducks-web-shader"),
        source: wgpu::ShaderSource::Wgsl(agnosis_core::render::BLIT_SHADER.into()),
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("ducks-web-layout"),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("ducks-web-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Wgpu {
    /// Re-lay the surface and the upload texture for a new frame size.
    fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        let (texture, texture_view) =
            upload_texture(&self.device, width, height, self.config.format);
        self.texture = texture;
        self.texture_view = texture_view;
        self.bind_group = make_bind_group(
            &self.device,
            &self.layout,
            &self.texture_view,
            &self.sampler,
        );
    }

    /// The next surface texture, or `None` when the frame is skipped.
    ///
    /// A timeout and an occluded surface skip the frame; a stale or a
    /// lost surface is reconfigured first and then skipped; a validation
    /// error is reported to the console and skipped.
    fn acquire(&mut self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Some(frame),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Some(frame),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => None,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                warn("the surface reported a validation error — the frame was skipped");
                None
            }
        }
    }

    /// Upload the frame into the texture.
    fn upload(&self, rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Draw one fullscreen quad and present it.
    fn draw(&self, frame: wgpu::SurfaceTexture) {
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("agnosis_web"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ducks-web-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);
    }
}

impl Renderer for Wgpu {
    fn present(&mut self, rgba: &[u8], width: u32, height: u32) {
        if !frame_is_valid(rgba, width, height) {
            warn("the frame does not match the frame size — the frame was skipped");
            return;
        }
        let limit = self.device.limits().max_texture_dimension_2d;
        if width > limit || height > limit {
            // WHY once: the warning was printed per frame, so a 4K
            // canvas on a driver with a 2048 limit flooded the console
            // (audit 2026-10-21 §W4).
            if !self.warned_oversized {
                self.warned_oversized = true;
                warn(&format!(
                    "the frame is {width}x{height}, larger than the texture limit {limit} — \
                     the frame was skipped (this is warned about once)"
                ));
            }
            return;
        }
        if width != self.width || height != self.height {
            self.resize(width, height);
        }
        let Some(frame) = self.acquire() else {
            return;
        };
        self.upload(rgba);
        self.draw(frame);
    }

    fn name(&self) -> &'static str {
        self.name
    }
}

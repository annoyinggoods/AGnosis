//! wgpu blit renderer: one fullscreen textured quad over the popup surface.
//!
//! Each frame the [`agnosis_core::grid::Grid`] is rasterized on the CPU through
//! [`agnosis_core::font::Font`] + [`agnosis_core::blit::blit_grid_rows_between`] into an
//! RGBA8 staging buffer, uploaded once with `queue.write_texture`, and drawn
//! as a single fullscreen textured quad. Frame pacing (30fps cap) is the
//! popup's job, not the renderer's.
//!
//! Adapter + device selection runs on a background thread via
//! [`spawn_device_thread`] so window show stays in the ~15-30ms budget; the
//! surface, texture and pipeline are built on the main thread once the device
//! is in hand.
//!
//! **Two layers** (roadmap `REVIEW/ROADMAP_DRAW_EDITOR.md` §4, decision D8,
//! Stage 5). The parade popup has one grid and blits it with one font
//! ([`Renderer::draw`], unchanged). The draw-your-own editor has two grids
//! with two cell sizes — the big-cell canvas layer and the small-cell UI
//! layer — so [`Renderer::draw_layers`] blits the base grid first and the
//! overlay grid on top of it. A space cell is transparent, so the base layer
//! shows through wherever the overlay has no ink, and the overlay's ink wins
//! wherever both have ink. The composition itself lives in the pure
//! [`blit_layers`] function, so it is testable without a GPU.

use agnosis_core::font::Font;
use agnosis_core::grid::Grid;
/// The fullscreen-quad WGSL.
///
/// ONE shader, two front ends: the native renderer and the web
/// renderer blit the core's frame through the **same** WGSL, and
/// two copies of it cannot drift-check each other (audit
/// 2026-10-21 §G32).
pub(crate) use agnosis_core::render::BLIT_SHADER;

/// Frames the GPU may queue ahead of the display. Two keeps the input
/// latency low while still absorbing a slow frame.
const MAX_FRAME_LATENCY: u32 = 2;

/// Outcome of one non-blocking GPU-init poll.
///
/// Both windows poll the same channel, so the outcome lives **here**:
/// one definition, two callers (audit 2026-10-21 §G26).
#[derive(Debug)]
pub(crate) enum GpuPoll {
    /// The device bundle arrived.
    Ready(GpuInit),
    /// Still initializing; poll again next frame.
    Pending,
    /// The sender vanished (or there is no channel): degrade.
    Gone,
}

/// The GPU life-cycle of one window (audit 101126 §S3): the device
/// thread's answer is awaited, the renderer is live, or the window is
/// degraded — announced once, honestly. One field instead of three
/// (`gpu_rx` + `gpu_dead` + `renderer`) makes the impossible combination
/// (a renderer beside a pending init, a dead window that re-announces)
/// unrepresentable. One definition, three windows: the popup, the
/// editor and HELP cannot drift apart.
pub(crate) enum Gpu {
    /// The device thread has not answered yet.
    Awaiting(std::sync::mpsc::Receiver<GpuInit>),
    /// The renderer is live; the window draws through it. Boxed: the
    /// renderer dwarfs the receiver, and the draw path dereferences it
    /// once per frame.
    Ready(Box<Renderer>),
    /// Init failed or the renderer could not be built: the window keeps
    /// running degraded (the parade advances its scene; HELP and the
    /// editor say so in their title).
    Dead,
}

/// One non-blocking poll of the GPU init channel (audit §poll_gpu): the arms
/// report the outcome and `poll_gpu` decides, so neither nests.
pub(crate) fn try_recv_init(rx: Option<&std::sync::mpsc::Receiver<GpuInit>>) -> GpuPoll {
    let rx = match rx {
        Some(rx) => rx,
        // No channel (already consumed): stay pending, matching the old
        // early return that left `gpu_dead` untouched.
        None => return GpuPoll::Pending,
    };
    match rx.try_recv() {
        Ok(init) => GpuPoll::Ready(init),
        Err(std::sync::mpsc::TryRecvError::Empty) => GpuPoll::Pending,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => GpuPoll::Gone,
    }
}

/// Renderer failures.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Device, surface or pipeline creation failed.
    #[error("render init failed: {0}")]
    Init(String),
    /// Surface acquire/configure failed fatally.
    #[error("surface failed: {0}")]
    Surface(String),
}

/// Bytes per pixel of the RGBA8 staging buffer.
pub const STAGING_BPP: usize = 4;

/// Staging buffer length in bytes for a `width x height` surface.
pub fn staging_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * STAGING_BPP
}

/// Build a present-ready surface configuration for `width x height`.
///
/// Prefers the first sRGB format the surface reports, otherwise the first
/// reported format. Prefers `Fifo` present mode. Returns `None` when the
/// surface reports no formats for the adapter.
pub fn pick_surface_config(
    surface: &wgpu::Surface<'_>,
    adapter: &wgpu::Adapter,
    width: u32,
    height: u32,
) -> Option<wgpu::SurfaceConfiguration> {
    let caps = surface.get_capabilities(adapter);
    let format = caps
        .formats
        .iter()
        .copied()
        .find(|f| f.is_srgb())
        .or_else(|| caps.formats.first().copied())?;
    let present = caps
        .present_modes
        .iter()
        .copied()
        .find(|m| *m == wgpu::PresentMode::Fifo)
        .or_else(|| caps.present_modes.first().copied())
        .unwrap_or(wgpu::PresentMode::Fifo);
    let alpha = caps
        .alpha_modes
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto);
    Some(wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Auto,
        width: width.max(1),
        height: height.max(1),
        present_mode: present,
        desired_maximum_frame_latency: MAX_FRAME_LATENCY,
        alpha_mode: alpha,
        view_formats: vec![],
    })
}

/// Bind group layout shared by the pipeline and the bind group.
fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("agnosis-blit-bind-layout"),
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

/// The upload texture's format, for a surface that reports `format`.
///
/// WHY it follows the **surface**: the upload texture is sampled by the
/// shader, so an sRGB texture is *decoded* to linear before it is
/// written to the surface. A surface that is **not** sRGB wants linear
/// values, so an sRGB texture made the on-screen picture **darker**
/// than the exported PNG and GIF — and nothing tested it (audit
/// 2026-10-21 §G32).
///
/// The web front end uploads the same RGBA bytes as a plain
/// `Rgba8Unorm` image, so the two pictures must match.
fn upload_format(format: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if format.is_srgb() {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

/// Allocate the RGBA8 upload texture plus its default view.
///
/// The texture's format follows the **surface's** format, so the
/// picture on the screen is the picture the exporters write (audit
/// 2026-10-21 §G32).
fn upload_texture(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    surface_format: wgpu::TextureFormat,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("agnosis-frame"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: upload_format(surface_format),
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Build the bind group for one upload-texture view + sampler pair.
fn make_bind_group(
    device: &wgpu::Device,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let layout = bind_group_layout(device);
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("agnosis-blit-bind"),
        layout: &layout,
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

/// Blit `base` and then `overlay` (on top of it) into `fb`.
///
/// Both grids are blitted at pixel origin `(0, 0)` and each one uses its own
/// cell size — the base grid's cell size for the base layer, the overlay
/// grid's for the overlay layer (AGENTS.md: a grid and its font must share
/// the cell size, or the glyphs land off-frame).
///
/// The overlay is blitted **after** the base, so its ink wins wherever both
/// layers have ink. A space cell is transparent, so the base layer stays
/// visible wherever the overlay has none (roadmap §4: "the overlay is
/// blitted after the base, so dropdown menus can overlap the canvas").
///
/// The function is pure — it only touches `fb` — so the two-layer
/// composition is testable without a GPU.
pub fn blit_layers(
    fb: &mut agnosis_core::blit::FrameBuffer,
    base: &Grid,
    base_font: &mut Font,
    overlay: Option<(&Grid, &mut Font)>,
) {
    let base_rows = base.rows();
    agnosis_core::blit::blit_grid_rows_between(base, base_font, fb, 0, 0..base_rows);
    let Some((over, over_font)) = overlay else {
        return;
    };
    let over_rows = over.rows();
    agnosis_core::blit::blit_grid_rows_between(over, over_font, fb, 0, 0..over_rows);
}

/// Blit renderer over an already-configured surface.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    bind_group: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    staging: Vec<u8>,
    width: u32,
    height: u32,
}

impl Renderer {
    /// Build from an already-created device/queue + surface on the main thread.
    ///
    /// Configures the surface, allocates the RGBA8 upload texture and staging
    /// buffer, and compiles the fullscreen-quad pipeline.
    pub fn new(
        device: wgpu::Device,
        queue: wgpu::Queue,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        if width == 0 || height == 0 {
            return Err(RenderError::Init("surface size must be nonzero".into()));
        }
        let mut config = config;
        config.width = width;
        config.height = height;
        surface.configure(&device, &config);
        let (texture, texture_view) = upload_texture(&device, width, height, config.format);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("agnosis-blit-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let bind_group = make_bind_group(&device, &texture_view, &sampler);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("agnosis-blit-shader"),
            source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("agnosis-blit-layout"),
            bind_group_layouts: &[Some(&bind_group_layout(&device))],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("agnosis-blit-pipeline"),
            layout: Some(&layout),
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
                    format: config.format,
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
        });
        Ok(Self {
            device,
            queue,
            surface,
            config,
            texture,
            texture_view,
            sampler,
            bind_group,
            pipeline,
            staging: vec![0u8; staging_len(width, height)],
            width,
            height,
        })
    }

    /// The last composed frame, as an owned copy of the staging buffer.
    ///
    /// The screenshot path's source: F12 copies the frame the user is
    /// **looking at** instead of rendering a new one on the event loop
    /// (audit 2026-10-23 §F-095). A skipped frame (a timeout, an
    /// occluded surface) leaves the previous frame in place, so this is
    /// always the last frame that was presented — never a half-blit.
    pub fn last_frame(&self) -> agnosis_core::blit::FrameBuffer {
        agnosis_core::blit::FrameBuffer {
            width: self.width,
            height: self.height,
            data: self.staging.clone(),
        }
    }

    /// Resize the swapchain and reallocate staging + upload texture.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.staging = vec![0u8; staging_len(width, height)];
        let (texture, view) = upload_texture(&self.device, width, height, self.config.format);
        self.texture = texture;
        self.texture_view = view;
        self.bind_group = make_bind_group(&self.device, &self.texture_view, &self.sampler);
    }

    /// Blit the grid via Font into the staging buffer, upload once with
    /// `queue.write_texture`, and draw one fullscreen quad.
    ///
    /// The sub-cell scroll `offset_px` applies only to the parade rows above
    /// `ground_row`; the grass/HUD/input rows from `ground_row` down are
    /// blitted at offset 0 so static content stays pixel-stable. That
    /// two-pass offset split is the whole reason the blitter takes a row
    /// range (audit 2026-10-17 § Maintainability: the old per-row uniform
    /// override is gone — the block font renders every on-screen glyph as
    /// the same `X` cell, so there is nothing left to normalize).
    ///
    /// The `layers` are the parallax layers, drawn under the scene grid
    /// with their own scroll offsets (roadmap Extension 4 §1.2). The
    /// composition itself is [`agnosis_core::blit::blit_scene`], the **same**
    /// function the headless gate uses, so the app and the exported
    /// frames cannot drift.
    ///
    /// `Outdated`/`Lost` surfaces reconfigure and skip the frame; timeouts
    /// and occlusion skip the frame; validation errors are hard errors.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        grid: &Grid,
        font: &mut Font,
        offset_px: i32,
        ground_row: usize,
        sky_ramp: Option<&[[u8; 3]]>,
        layers: &[agnosis_core::layer::LayerBlit<'_>],
        overlay: Option<&agnosis_core::layer::LayerBlit<'_>>,
    ) -> Result<(), RenderError> {
        let Some(frame) = self.acquire_frame()? else {
            return Ok(());
        };
        let mut fb = self.take_framebuffer();
        agnosis_core::blit::blit_scene(
            &mut fb, font, grid, offset_px, ground_row, sky_ramp, layers, overlay,
        );
        self.staging = std::mem::take(&mut fb.data);
        self.present(frame);
        Ok(())
    }

    /// Blit `base` and then `overlay` (on top of it) into the staging
    /// buffer, upload once and draw one fullscreen quad.
    ///
    /// This is the editor's path (roadmap Stage 5): the base grid carries
    /// the big-cell canvas and tool panel, the overlay grid the small-cell
    /// UI (dropdowns, name field, status line). Both are blitted at pixel
    /// origin `(0, 0)`, each with its own cell size; the overlay's ink wins
    /// over the base's and a transparent overlay cell leaves the base
    /// visible. The composition itself is the pure [`blit_layers`].
    ///
    /// The surface handling is identical to [`Renderer::draw`]: a stale
    /// surface reconfigures and skips the frame, a timeout or an occluded
    /// surface skips it, a validation error is a hard error.
    pub fn draw_layers(
        &mut self,
        base: &Grid,
        base_font: &mut Font,
        overlay: Option<(&Grid, &mut Font)>,
    ) -> Result<(), RenderError> {
        let Some(frame) = self.acquire_frame()? else {
            return Ok(());
        };
        let mut fb = self.take_framebuffer();
        fb.clear_black();
        blit_layers(&mut fb, base, base_font, overlay);
        self.staging = std::mem::take(&mut fb.data);
        self.present(frame);
        Ok(())
    }

    /// Blit a **text** base layer (and its overlay, on top of it) and
    /// present one fullscreen quad.
    ///
    /// This is the HELP window's path (roadmap HELP_WINDOW §1): the base
    /// grid carries README prose that renders through the fixed-scale
    /// body-text blit ([`agnosis_core::blit::blit_text_between`]), the
    /// overlay grid the block-font footer, which keeps the ordinary
    /// ink-fit path. The surface handling is [`Renderer::draw_layers`]'s.
    pub fn draw_text_layers(
        &mut self,
        base: &Grid,
        base_font: &mut Font,
        overlay: Option<(&Grid, &mut Font)>,
    ) -> Result<(), RenderError> {
        let Some(frame) = self.acquire_frame()? else {
            return Ok(());
        };
        let mut fb = self.take_framebuffer();
        fb.clear_black();
        let base_rows = base.rows();
        agnosis_core::blit::blit_text_between(base, base_font, &mut fb, 0, 0..base_rows);
        let Some((over, over_font)) = overlay else {
            self.staging = std::mem::take(&mut fb.data);
            self.present(frame);
            return Ok(());
        };
        let over_rows = over.rows();
        agnosis_core::blit::blit_grid_rows_between(over, over_font, &mut fb, 0, 0..over_rows);
        self.staging = std::mem::take(&mut fb.data);
        self.present(frame);
        Ok(())
    }

    /// Acquire the next surface texture.
    ///
    /// `Ok(None)` means "skip this frame": the surface timed out, was
    /// occluded, or went stale (in which case it is reconfigured first).
    /// A validation error is the only hard failure.
    fn acquire_frame(&mut self) -> Result<Option<wgpu::SurfaceTexture>, RenderError> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => Ok(Some(frame)),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => Ok(Some(frame)),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                Ok(None)
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                Err(RenderError::Surface("surface validation error".into()))
            }
        }
    }

    /// Take the staging buffer out of the renderer for blitting.
    fn take_framebuffer(&mut self) -> agnosis_core::blit::FrameBuffer {
        agnosis_core::blit::FrameBuffer {
            width: self.width,
            height: self.height,
            data: std::mem::take(&mut self.staging),
        }
    }

    /// Upload the staging buffer and present one fullscreen quad.
    ///
    /// Shared by [`Renderer::draw`] and [`Renderer::draw_layers`] so the
    /// upload/present path cannot drift between the one-layer and the
    /// two-layer renderer.
    fn present(&mut self, frame: wgpu::SurfaceTexture) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &self.staging,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * STAGING_BPP as u32),
                rows_per_image: Some(self.height),
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
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

/// Single-flight GPU init bundle.
///
/// One background thread creates one [`wgpu::Instance`], requests one
/// adapter from it and one device from that adapter, then ships the whole
/// bundle to the main thread. The surface and renderer are built there from
/// this same instance/adapter pair, so wgpu-core never sees a foreign
/// `AdapterId`: adapters and surfaces from different instances panic inside
/// wgpu-core storage.
#[derive(Debug)]
pub struct GpuInit {
    /// Owning instance; surface creation happens on the main thread from it.
    pub instance: wgpu::Instance,
    /// Adapter the device came from; also serves surface capabilities.
    pub adapter: wgpu::Adapter,
    /// Ready device for the renderer.
    pub device: wgpu::Device,
    /// Queue paired with `device`.
    pub queue: wgpu::Queue,
}

/// Spawn the single background GPU init thread. Sends one [`GpuInit`]
/// bundle on success and goes quiet otherwise. Uses `HighPerformance`
/// power preference and never touches the window or surface.
pub fn spawn_device_thread() -> std::sync::mpsc::Receiver<GpuInit> {
    let (tx, rx) = std::sync::mpsc::channel();
    if std::thread::Builder::new()
        .name("agnosis-wgpu-init".into())
        .spawn(move || {
            let instance = wgpu::Instance::default();
            let adapter =
                pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: None,
                    force_fallback_adapter: false,
                    apply_limit_buckets: false,
                }));
            let Ok(adapter) = adapter else {
                agnosis_core::trace::trace("wgpu.init no-adapter");
                return;
            };
            let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("agnosis-popup"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
                ..Default::default()
            }));
            match device {
                Ok((device, queue)) => {
                    agnosis_core::trace::trace("wgpu.init device-ready");
                    let _ = tx.send(GpuInit {
                        instance,
                        adapter,
                        device,
                        queue,
                    });
                }
                Err(e) => {
                    agnosis_core::trace::trace(&format!("wgpu.init device-failed: {e}"));
                }
            }
        })
        .is_err()
    {
        agnosis_core::trace::trace("wgpu.init thread-spawn-failed");
    }
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_len_matches_rgba8_math() {
        assert_eq!(staging_len(1920, 1080), 1920 * 1080 * 4);
        assert_eq!(staging_len(1, 1), 4);
        assert_eq!(staging_len(160 * 12, 45 * 24), 1920 * 1080 * 4);
    }

    /// The upload texture's colour space follows the **surface's**.
    ///
    /// An sRGB surface with a linear upload texture (or the other way
    /// round) made the on-screen picture **darker** than the exported
    /// PNG and GIF, and nothing tested it (audit 2026-10-21 §G32).
    #[test]
    fn the_upload_format_follows_the_surface_format() {
        assert_eq!(
            upload_format(wgpu::TextureFormat::Rgba8UnormSrgb),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            upload_format(wgpu::TextureFormat::Bgra8UnormSrgb),
            wgpu::TextureFormat::Rgba8UnormSrgb
        );
        assert_eq!(
            upload_format(wgpu::TextureFormat::Rgba8Unorm),
            wgpu::TextureFormat::Rgba8Unorm
        );
        assert_eq!(
            upload_format(wgpu::TextureFormat::Bgra8Unorm),
            wgpu::TextureFormat::Rgba8Unorm
        );
    }

    #[test]
    fn blit_shader_is_nonempty_wgsl() {
        assert!(BLIT_SHADER.contains("vs_main"));
        assert!(BLIT_SHADER.contains("fs_main"));
        assert!(BLIT_SHADER.contains("textureSample"));
    }

    #[test]
    fn spawn_device_thread_never_panics_or_hangs() {
        let rx = spawn_device_thread();
        // The bundle may never arrive on a GPU-less box; the gate is that the
        // call returns instead of hanging.
        let _ = rx.recv_timeout(std::time::Duration::from_secs(25));
    }

    #[test]
    fn single_flight_bundle_carries_matching_adapter() {
        let rx = spawn_device_thread();
        if let Ok(init) = rx.recv_timeout(std::time::Duration::from_secs(25)) {
            let info = init.adapter.get_info();
            assert!(!format!("{:?}", info.backend).is_empty());
            let _ = (&init.instance, &init.device, &init.queue);
        }
    }

    /// True when any pixel of the 4x4 cell at `(x0, y0)` carries exactly
    /// `color`. Split from the compositing test so no loop nests a loop.
    fn cell_has_ink(
        fb: &agnosis_core::blit::FrameBuffer,
        x0: u32,
        y0: u32,
        color: [u8; 3],
    ) -> bool {
        (0..4)
            .flat_map(|dy| (0..4).map(move |dx| (dx, dy)))
            .any(|(dx, dy)| {
                let px = fb.pixel(x0 + dx, y0 + dy);
                px[0] == color[0] && px[1] == color[1] && px[2] == color[2]
            })
    }

    #[test]
    fn draw_layers_composites_two_grids() {
        // Two 2x1-cell grids at the same 4x4 cell size: the base carries a
        // red cell and a blue cell, the overlay a green cell over the red
        // one and a transparent (space) cell over the blue one.
        let mut base = Grid::new(2, 1, 4, 4);
        base.set(0, 0, 'X', [255, 0, 0]);
        base.set(1, 0, 'X', [0, 0, 255]);
        let mut overlay = Grid::new(2, 1, 4, 4);
        overlay.set(0, 0, 'X', [0, 255, 0]);
        // Cell (1,0) of the overlay stays a space: a space is transparent.
        let mut base_font = Font::new(4, 4).expect("font");
        let mut overlay_font = Font::new(4, 4).expect("font");
        let mut fb = agnosis_core::blit::FrameBuffer::new(8, 4);
        fb.clear_black();
        blit_layers(
            &mut fb,
            &base,
            &mut base_font,
            Some((&overlay, &mut overlay_font)),
        );
        // The overlay wins where both layers have ink.
        assert!(
            cell_has_ink(&fb, 0, 0, [0, 255, 0]),
            "the overlay must win over the base at the same pixel"
        );
        assert!(
            !cell_has_ink(&fb, 0, 0, [255, 0, 0]),
            "the covered base ink must be gone"
        );
        // A transparent overlay cell leaves the base visible.
        assert!(
            cell_has_ink(&fb, 4, 0, [0, 0, 255]),
            "a space overlay cell must leave the base visible"
        );
        assert!(
            !cell_has_ink(&fb, 4, 0, [0, 255, 0]),
            "a space overlay cell must not ink"
        );
    }

    #[test]
    fn fallback_format_config_shape() {
        let cfg = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: wgpu::TextureFormat::Bgra8UnormSrgb,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: 1920,
            height: 1080,
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: MAX_FRAME_LATENCY,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: vec![],
        };
        assert_eq!(cfg.width, 1920);
        assert_eq!(cfg.height, 1080);
        assert_eq!(staging_len(cfg.width, cfg.height), 1920 * 1080 * 4);
    }
}

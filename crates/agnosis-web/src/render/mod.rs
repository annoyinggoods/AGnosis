//! The drawing back ends: WebGPU, WebGL2 and the 2D canvas fallback.
//!
//! Three back ends sit behind one [`Renderer`] trait, so the parade shows
//! in **every** browser:
//!
//! * **WebGPU** — `wgpu` 30's browser WebGPU backend.
//! * **WebGL2** — `wgpu` 30's browser WebGL2 backend (`wgpu-hal`'s
//!   `gles/web.rs`). That is the `webgl` **feature**; the `gles` feature
//!   is the *native* GLES path and has nothing to do with the browser.
//! * **2D canvas** — the fallback that always works: the RGBA frame is
//!   uploaded with `putImageData`, and no GPU is involved at all.
//!
//! The frame the back ends draw is the core's own [`FrameBuffer`]: RGBA8,
//! top-down rows, no padding — `width * height * 4` bytes.
//!
//! # One canvas element, one context
//!
//! A canvas element carries **one** context type for its whole life: once
//! `getContext("webgpu")`, `getContext("webgl2")` or `getContext("2d")`
//! has succeeded, every other call returns `null`. The three back ends
//! are therefore **mutually exclusive per canvas element**, and a page
//! that switches back end has to hand [`create`] a **fresh** `<canvas>`
//! element (`document.createElement("canvas")`) — the old one cannot be
//! reused, not even for the same back end.
//!
//! [`create`] does not waste the canvas it is given:
//!
//! * the WebGPU back end asks for its **adapter and its device before it
//!   creates the surface**, and the surface is what takes the `webgpu`
//!   context — so a browser without WebGPU leaves the canvas untouched
//!   and the chain can carry on;
//! * the WebGL2 back end has to create the surface **before** the adapter
//!   (the WebGL2 backend enumerates adapters *from* a surface, so
//!   `compatible_surface` is mandatory), which takes the `webgl2`
//!   context early. The chain therefore probes the browser's WebGL2
//!   support on a **throwaway** canvas first and only spends the real
//!   canvas when WebGL2 is there to be had.
//!
//! When a canvas is spent anyway, [`create`] says so in plain words
//! instead of drawing a black rectangle.

use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;

mod canvas2d;
#[cfg(target_arch = "wasm32")]
mod webgpu;

/// Which back end the page should use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Try WebGPU, then WebGL2, then the plain 2D canvas.
    Auto,
    /// The browser's WebGPU.
    WebGpu,
    /// WebGL2.
    WebGl2,
    /// The plain 2D canvas — the fallback that always works.
    Canvas2d,
}

/// A drawing back end.
pub trait Renderer {
    /// Draw one frame.
    ///
    /// `rgba` is `width * height * 4` bytes: RGBA8, top-down rows, no
    /// padding — the core's `FrameBuffer` byte for byte.
    fn present(&mut self, rgba: &[u8], width: u32, height: u32);
    /// A short name for the page's status line ("webgpu", "webgl2", "canvas2d").
    fn name(&self) -> &'static str;
}

/// Build a renderer for `backend` on `canvas`.
///
/// `Auto` tries WebGPU, then WebGL2, then the 2D canvas. The error is a
/// human-readable string for the page.
///
/// # The canvas is spent by the first back end that takes it
///
/// A canvas element carries one context type for its whole life, so
/// `canvas` must be a **fresh** element whenever the page switches back
/// end. `create` never takes a context it does not need, but once a back
/// end has taken one the element cannot serve another: the error says so
/// and asks for a new element rather than leaving a black canvas behind.
pub async fn create(
    canvas: &HtmlCanvasElement,
    backend: Backend,
) -> Result<Box<dyn Renderer>, String> {
    if backend == Backend::Auto {
        return create_auto(canvas).await;
    }
    match try_backend(canvas, backend).await {
        Ok(renderer) => Ok(renderer),
        Err(attempt) => Err(format!("{}: {}", label(backend), attempt.message)),
    }
}

/// The back ends `Auto` tries, in order.
///
/// The order is the *table of contents* of the fall-back chain: WebGPU is
/// the fastest, WebGL2 is the one every browser has, and the 2D canvas
/// is the one that always works.
const AUTO_ORDER: [Backend; 3] = [Backend::WebGpu, Backend::WebGl2, Backend::Canvas2d];

/// Try the back ends of [`AUTO_ORDER`] until one of them works.
///
/// Every failure is collected, so the page's status line can say what
/// was tried and why each step gave up.
async fn create_auto(canvas: &HtmlCanvasElement) -> Result<Box<dyn Renderer>, String> {
    let mut notes: Vec<String> = Vec::new();
    for kind in AUTO_ORDER {
        if !should_try(kind) {
            notes.push(format!("{}: not available in this browser", label(kind)));
            continue;
        }
        let attempt = match try_backend(canvas, kind).await {
            Ok(renderer) => return Ok(renderer),
            Err(attempt) => attempt,
        };
        notes.push(format!("{}: {}", label(kind), attempt.message));
        if attempt.canvas_taken {
            return Err(spent_canvas_message(&notes));
        }
    }
    Err(format!(
        "no back end could be created: {}",
        notes.join("; ")
    ))
}

/// Whether the chain should spend the canvas on `kind` at all.
///
/// The WebGL2 probe runs on a **throwaway** canvas: a real canvas that
/// already carries a `webgl2` context cannot be reused, and one that has
/// been given another context type cannot get one at all. Asking a fresh
/// element keeps the parade's own canvas free for the 2D fallback.
fn should_try(kind: Backend) -> bool {
    match kind {
        Backend::WebGl2 => webgl2_available(),
        _ => true,
    }
}

/// Try one back end.
async fn try_backend(
    canvas: &HtmlCanvasElement,
    kind: Backend,
) -> Result<Box<dyn Renderer>, Attempt> {
    match kind {
        Backend::WebGpu | Backend::WebGl2 => create_wgpu(canvas, kind).await,
        Backend::Canvas2d => canvas2d::create(canvas).map(|r| Box::new(r) as Box<dyn Renderer>),
        Backend::Auto => Err(Attempt::failed("`Auto` is not a single back end")),
    }
}

/// Build the wgpu back ends. The browser is the only place they exist.
#[cfg(target_arch = "wasm32")]
async fn create_wgpu(
    canvas: &HtmlCanvasElement,
    kind: Backend,
) -> Result<Box<dyn Renderer>, Attempt> {
    webgpu::create(canvas, kind)
        .await
        .map(|r| Box::new(r) as Box<dyn Renderer>)
}

/// The wgpu back ends need a browser; on the host there is none.
#[cfg(not(target_arch = "wasm32"))]
async fn create_wgpu(
    _canvas: &HtmlCanvasElement,
    _kind: Backend,
) -> Result<Box<dyn Renderer>, Attempt> {
    Err(Attempt::failed(
        "the WebGPU and WebGL2 back ends need a browser (a wasm32 build)",
    ))
}

/// One back end's failure, and what it means for the canvas.
pub(crate) struct Attempt {
    /// The human-readable reason.
    pub(crate) message: String,
    /// The canvas element already carries a context, so **no other back
    /// end can use it** and the page has to build a fresh element.
    pub(crate) canvas_taken: bool,
}

impl Attempt {
    /// The back end gave up, and the canvas is still free.
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            canvas_taken: false,
        }
    }

    /// The back end gave up **after** it took the canvas' context.
    pub(crate) fn spent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            canvas_taken: true,
        }
    }
}

/// The message for a canvas that is out of contexts.
fn spent_canvas_message(notes: &[String]) -> String {
    format!(
        "{} — the canvas element already carries a context, and a canvas element has one \
         context type for its whole life: build a fresh <canvas> element \
         (document.createElement) and hand that to create() to switch back end",
        notes.join("; ")
    )
}

/// The short name of a back end — the same string the status line shows.
pub(crate) fn label(kind: Backend) -> &'static str {
    match kind {
        Backend::Auto => "auto",
        Backend::WebGpu => "webgpu",
        Backend::WebGl2 => "webgl2",
        Backend::Canvas2d => "canvas2d",
    }
}

/// Bytes per pixel of the RGBA8 frame buffer.
pub(crate) const BYTES_PER_PIXEL: usize = 4;

/// The byte length of an RGBA8 frame of `width` × `height` pixels.
pub(crate) fn rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * BYTES_PER_PIXEL
}

/// True when `rgba` is exactly one RGBA8 frame of `width` × `height`.
///
/// The back ends check this before they touch the GPU: a frame that does
/// not match its size is **skipped**, never guessed at.
pub(crate) fn frame_is_valid(rgba: &[u8], width: u32, height: u32) -> bool {
    width > 0 && height > 0 && rgba.len() == rgba_len(width, height)
}

/// The upload texture's format, for a surface that reports `format`.
///
/// WHY it follows the **surface**: the upload texture is *sampled* by
/// the shader, so an sRGB texture is decoded to linear on the way in,
/// and an sRGB surface encodes it back to sRGB on the way out. The
/// two cancel and the picture is the frame's own bytes.
///
/// A **linear** surface does neither, so it wants the frame's bytes as
/// they are — which is what a plain `Rgba8Unorm` upload texture gives
/// it. That is the WebGPU back end's case: the browser's WebGPU canvas
/// has no sRGB format at all.
///
/// The native renderer has the same rule (`src/render.rs`); the web
/// back ends differ in their surface format, so the upload format
/// follows it per back end.
///
/// It is `pub` because the wgpu back end is `#[cfg(target_arch =
/// "wasm32")]`: a host build of this library never calls it, and a
/// `pub` item in a library cannot be dead code (audit 2026-10-21 §F9).
pub fn upload_format(format: wgpu::TextureFormat) -> wgpu::TextureFormat {
    if format.is_srgb() {
        wgpu::TextureFormat::Rgba8UnormSrgb
    } else {
        wgpu::TextureFormat::Rgba8Unorm
    }
}

/// True when the browser can give a canvas a `webgl2` context.
///
/// The probe runs on a **throwaway** canvas (see [`should_try`]).
fn webgl2_available() -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    let Some(document) = window.document() else {
        return false;
    };
    let Ok(element) = document.create_element("canvas") else {
        return false;
    };
    let Ok(canvas) = element.dyn_into::<HtmlCanvasElement>() else {
        return false;
    };
    canvas.get_context("webgl2").ok().flatten().is_some()
}

/// Print one line to the browser's console.
///
/// A frame that cannot be drawn is rare and worth a console line, but it
/// must never take the parade down — the next frame may well be fine.
pub(crate) fn warn(message: &str) {
    web_sys::console::warn_1(&wasm_bindgen::JsValue::from_str(message));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_is_copy_and_comparable() {
        let auto = Backend::Auto;
        let copied = auto;
        assert_eq!(auto, copied, "Backend must be Copy");
        assert_eq!(Backend::Auto, Backend::Auto);
        assert_ne!(Backend::Auto, Backend::WebGpu);
        assert_ne!(Backend::WebGpu, Backend::WebGl2);
        assert_ne!(Backend::WebGl2, Backend::Canvas2d);
        assert_ne!(Backend::Canvas2d, Backend::Auto);
    }

    #[test]
    fn auto_tries_webgpu_then_webgl2_then_the_canvas() {
        assert_eq!(
            AUTO_ORDER,
            [Backend::WebGpu, Backend::WebGl2, Backend::Canvas2d]
        );
    }

    #[test]
    fn every_backend_has_a_status_line_label() {
        assert_eq!(label(Backend::WebGpu), "webgpu");
        assert_eq!(label(Backend::WebGl2), "webgl2");
        assert_eq!(label(Backend::Canvas2d), "canvas2d");
        assert_eq!(label(Backend::Auto), "auto");
    }

    #[test]
    fn the_rgba_layout_is_four_bytes_per_pixel() {
        assert_eq!(rgba_len(0, 0), 0);
        assert_eq!(rgba_len(1, 1), 4);
        assert_eq!(rgba_len(2, 3), 2 * 3 * 4);
        assert_eq!(rgba_len(1920, 1080), 1920 * 1080 * 4);
        assert_eq!(rgba_len(3840, 2160), 3840 * 2160 * 4);
    }

    /// The upload texture's colour space follows the **surface's**.
    ///
    /// The WebGL2 back end's surface is sRGB (so the swapchain encodes
    /// linear to sRGB on write) and needs an sRGB upload texture to
    /// cancel it; the browser's WebGPU canvas has no sRGB format and
    /// needs the bytes as they are. A mismatch in either direction
    /// shifts every colour (audit 2026-10-21 § web colour fidelity).
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
    fn a_frame_must_match_its_size() {
        assert!(frame_is_valid(&[0; 4], 1, 1));
        assert!(frame_is_valid(&[0; 2 * 3 * 4], 2, 3));
        assert!(
            !frame_is_valid(&[0; 3], 1, 1),
            "a short frame is not a frame"
        );
        assert!(
            !frame_is_valid(&[0; 8], 1, 1),
            "a long frame is not a frame"
        );
        assert!(
            !frame_is_valid(&[0; 0], 0, 0),
            "an empty frame is not a frame"
        );
        assert!(!frame_is_valid(&[], 1, 0), "a zero height is not a frame");
    }

    #[test]
    fn a_spent_canvas_is_told_apart_from_a_free_one() {
        let free = Attempt::failed("no adapter");
        assert!(!free.canvas_taken);
        let spent = Attempt::spent("the canvas already has a context");
        assert!(spent.canvas_taken);
        assert!(
            spent_canvas_message(&["webgpu: no adapter".to_string()]).contains("fresh <canvas>")
        );
    }
}

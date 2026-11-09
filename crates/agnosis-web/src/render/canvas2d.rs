//! The plain 2D canvas back end: the fallback that always works.
//!
//! The frame is uploaded with `putImageData`: the RGBA buffer is handed
//! to an `ImageData` and blitted onto the canvas' backing store. No GPU
//! is involved, so this path works in every browser — and on a machine
//! without one at all.
//!
//! The canvas' backing store **is** the frame: the page scales the
//! element with CSS, so the buffer holds exactly the grid's pixels and
//! nothing is stretched by the blit.

use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlCanvasElement, ImageData};

use super::{frame_is_valid, warn, Attempt, Renderer};

/// The plain 2D canvas back end.
pub(crate) struct Canvas2d {
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
}

impl Canvas2d {
    /// Take the canvas' 2D context.
    ///
    /// A canvas element carries **one** context type for its whole life,
    /// so a canvas that already has a `webgpu` or a `webgl2` context
    /// cannot be used here: `getContext("2d")` returns `null` and the
    /// caller has to hand us a fresh element.
    pub(crate) fn new(canvas: &HtmlCanvasElement) -> Result<Self, Attempt> {
        let context = match canvas.get_context("2d") {
            Ok(Some(context)) => context,
            Ok(None) => {
                return Err(Attempt::spent(
                    "the canvas element already carries a context (an element has one context \
                     type for its whole life) — hand create() a fresh <canvas> element",
                ))
            }
            Err(error) => {
                return Err(Attempt::failed(format!(
                    "getContext(\"2d\") threw: {error:?}"
                )))
            }
        };
        let ctx = context
            .dyn_into::<CanvasRenderingContext2d>()
            .map_err(|_| Attempt::failed("the 2d context has an unexpected type"))?;
        Ok(Self {
            canvas: canvas.clone(),
            ctx,
        })
    }

    /// The frame as an `ImageData`.
    ///
    /// `ImageData` copies the bytes into a JS `Uint8ClampedArray`, so
    /// the frame buffer is handed over by reference and the copy happens
    /// on the JavaScript side.
    fn image(&self, rgba: &[u8], width: u32, height: u32) -> Option<ImageData> {
        let clamped = wasm_bindgen::Clamped(rgba);
        match ImageData::new_with_u8_clamped_array_and_sh(clamped, width, height) {
            Ok(image) => Some(image),
            Err(error) => {
                warn(&format!("the ImageData could not be built: {error:?}"));
                None
            }
        }
    }
}

/// Build the 2D canvas back end on `canvas`.
pub(crate) fn create(canvas: &HtmlCanvasElement) -> Result<Canvas2d, Attempt> {
    Canvas2d::new(canvas)
}

impl Renderer for Canvas2d {
    fn present(&mut self, rgba: &[u8], width: u32, height: u32) {
        if !frame_is_valid(rgba, width, height) {
            warn("the frame does not match the frame size — the frame was skipped");
            return;
        }
        // Setting the width or the height resets the canvas, so it only
        // happens when the size really changed.
        if self.canvas.width() != width {
            self.canvas.set_width(width);
        }
        if self.canvas.height() != height {
            self.canvas.set_height(height);
        }
        let Some(image) = self.image(rgba, width, height) else {
            return;
        };
        if let Err(error) = self.ctx.put_image_data(&image, 0.0, 0.0) {
            warn(&format!("putImageData failed: {error:?}"));
        }
    }

    fn name(&self) -> &'static str {
        "canvas2d"
    }
}

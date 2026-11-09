//! The ducks ASCII duck parade, in a browser tab.
//!
//! This crate is the **second front end** over the same core the native
//! app runs (roadmap `REVIEW/ROADMAP_EXTENSION_LATER.md` §1.2): the
//! simulation, the art, the physics and the exported frames are
//! `agnosis_core`'s, byte for byte. The web owns the canvas, the clock
//! and the mouse — and nothing else.
//!
//! The exported surface is deliberately small: the page starts the
//! parade, asks which back end is running, switches it, pauses it,
//! pokes a duck and hands over a manifest or a package.
//!
//! * [`app`] — the frame loop, the canvas and the clock.
//! * [`assets_io`] — the drawn art in memory and the two imports.
//! * [`input`] — the mouse.
//! * [`render`] — the drawing back ends (WebGPU, WebGL2, the 2D canvas).

mod app;
mod assets_io;
mod input;
pub mod render;

use wasm_bindgen::prelude::*;

/// Start the parade.
///
/// The wasm-bindgen `start` hook runs when the module is instantiated,
/// so the page does not have to call anything to see the ducks.
#[wasm_bindgen(start)]
pub fn start() {
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = app::boot().await {
            report(&format!("the parade could not start: {e}"));
        }
    });
}

/// Which back end is running: "webgpu", "webgl2", "canvas2d" — or
/// "none" before the first one is built.
#[wasm_bindgen]
pub fn backend_name() -> String {
    app::with_app(|state| {
        state
            .renderer
            .as_ref()
            .map(|renderer| renderer.name().to_string())
            .unwrap_or_else(|| "none".to_string())
    })
    .unwrap_or_else(|| "none".to_string())
}

/// Switch the drawing back end at runtime.
///
/// The name is one of "auto", "webgpu", "webgl2", "canvas2d" — the page
/// has a selector, and the forced choice is how the fallback is
/// exercised (roadmap §4.2). The switch itself is asynchronous (a
/// WebGPU adapter is requested), so the call returns as soon as the
/// request is made; the status line says what happened.
#[wasm_bindgen]
pub fn set_backend(name: &str) -> Result<(), String> {
    let backend = app::parse_backend(name).ok_or_else(|| format!("unknown back end {name:?}"))?;
    app::request_backend(backend);
    Ok(())
}

/// Pause the parade (or resume it).
#[wasm_bindgen]
pub fn set_paused(paused: bool) {
    app::with_app(|state| {
        state.paused = paused;
    });
}

/// React to a click at a page coordinate.
///
/// `x` and `y` are **page** coordinates (as `MouseEvent.client_x` and
/// `client_y` give them). A plain click makes the duck **jump** — it
/// keeps marching — and a shift-click **holds** it in place. Returns
/// whether a duck was hit.
#[wasm_bindgen]
pub fn poke_duck(x: f32, y: f32, shift: bool) -> bool {
    app::with_app(|state| input::poke(state, x, y, shift)).unwrap_or(false)
}

/// Apply a manifest — the `agnosis config export` output — to the parade.
///
/// Returns a one-line summary of what was applied.
#[wasm_bindgen]
pub fn apply_manifest(json: &str) -> Result<String, String> {
    assets_io::apply_manifest(json)
}

/// Read a `.agnosis` package — the `agnosis export` output — and apply it.
///
/// The bytes are the store-only ZIP the native app writes: the manifest
/// plus the drawn documents. Returns a one-line summary of what was
/// applied.
#[wasm_bindgen]
pub fn load_package(bytes: &[u8]) -> Result<String, String> {
    assets_io::load_package(bytes)
}

/// The one-line summary for the page's status bar.
#[wasm_bindgen]
pub fn status_line() -> String {
    app::with_app(|state| app::status_line(state)).unwrap_or_else(|| "starting".to_string())
}

/// Show a message in the page's diagnostic box.
///
/// The box is what a headless smoke test reads: a wasm build that
/// compiles but fails at runtime leaves its failure in the DOM.
fn report(message: &str) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(element) = document.get_element_by_id("diag") else {
        return;
    };
    let mut text = element.text_content().unwrap_or_default();
    text.push_str(message);
    text.push('\n');
    element.set_text_content(Some(&text));
}

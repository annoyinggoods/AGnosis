//! The mouse: the web build's best feature (roadmap Extension LATER §2).
//!
//! A browser has a pointer, so the parade can be **touched**: a click
//! on a duck makes it **jump** — it keeps marching — and a shift-click
//! **holds** it in place. This is the one thing the web does that the
//! native app cannot do as well.
//!
//! The chain is the core's: a page coordinate becomes a grid cell, the
//! cell becomes a train slot ([`Scene::duck_at`]), and the slot becomes
//! a reaction ([`Scene::react`]). The web only does the arithmetic.

use std::cell::RefCell;
use std::rc::Rc;

use agnosis_core::reaction::ReactionKind;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, MouseEvent};

use crate::app::{keep_alive, AppState};

/// Listen for clicks on the canvas.
pub fn install(app: &Rc<RefCell<AppState>>, canvas: &HtmlCanvasElement) -> Result<(), String> {
    let app = Rc::clone(app);
    let closure = Closure::<dyn FnMut(MouseEvent)>::new(move |event: MouseEvent| {
        let shift = event.shift_key();
        let x = event.client_x() as f32;
        let y = event.client_y() as f32;
        let mut state = app.borrow_mut();
        poke(&mut state, x, y, shift);
    });
    canvas
        .add_event_listener_with_callback("click", closure.as_ref().unchecked_ref())
        .map_err(|e| format!("cannot listen for clicks: {e:?}"))?;
    keep_alive(closure);
    Ok(())
}

/// Turn a page coordinate into a duck reaction.
///
/// `client_x` and `client_y` are **page** coordinates (as
/// `MouseEvent.client_x` gives them). A plain click makes the duck
/// jump; a shift-click holds it. Returns whether a duck was hit.
pub fn poke(state: &mut AppState, client_x: f32, client_y: f32, shift: bool) -> bool {
    let Some((col, row)) = cell_at(state, client_x, client_y) else {
        return false;
    };
    let Some(slot) = state.sim.scene.duck_at(col, row) else {
        return false;
    };
    let kind = if shift {
        ReactionKind::Stop
    } else {
        ReactionKind::Jump
    };
    state.sim.scene.react(slot, kind);
    true
}

/// The grid cell under a page coordinate.
///
/// The canvas element is scaled by the CSS (the drawing buffer holds
/// the grid's pixels, the element fills the window), so the page
/// coordinate is scaled by the ratio of the two — not by the device
/// pixel ratio, which would be wrong on a scaled page.
fn cell_at(state: &AppState, client_x: f32, client_y: f32) -> Option<(i64, i64)> {
    let rect = state.canvas.get_bounding_client_rect();
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    let scale_x = state.sim.fb.width as f64 / rect.width();
    let scale_y = state.sim.fb.height as f64 / rect.height();
    let x = (client_x as f64 - rect.left()) * scale_x;
    let y = (client_y as f64 - rect.top()) * scale_y;
    let col = (x / state.sim.cell_w.max(1) as f64).floor() as i64;
    let row = (y / state.sim.cell_h.max(1) as f64).floor() as i64;
    Some((col, row))
}

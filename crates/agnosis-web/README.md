# ducks-web — the parade, in a browser tab

The **second front end** over the same core the native app runs
(`crates/ducks-core`). The simulation, the art, the physics and the
grid are the core's, byte for byte; this crate owns the canvas, the
clock and the mouse, and nothing else (roadmap
`REVIEW/ROADMAP_EXTENSION_LATER.md` §1.2, §2).

```
crates/ducks-core/     the pure core (the simulation, the art, the exports)
crates/ducks-web/      this crate: the wasm-bindgen surface, the loop, the page
```

## Build

```bash
./crates/ducks-web/build-web.sh            # -> crates/ducks-web/dist
./crates/ducks-web/build-web.sh /tmp/ducks # -> somewhere else
```

The script builds the module with cargo and then runs the **wasm-bindgen
CLI** (`--target web --no-typescript`), falling back to **wasm-pack**
when the CLI is not installed. The wasm-bindgen crate and the CLI must
be the same version — `0.2.129` — because the CLI refuses a mismatched
module. The output folder holds three files:

```
index.html        the page
ducks_web.js      the wasm-bindgen glue
ducks_web_bg.wasm the module
```

Any static host serves them. There is **no** server-side component and
the page fetches **nothing** from the network.

> `file://` does not work in Chromium: an ES module cannot be imported
> from the file system. Serve the folder over http (`python3 -m
> http.server`) and it runs.

## The page

* the canvas fills the window; the drawing buffer holds the grid's
  pixels exactly, so the browser scales the picture and the cell pitch
  is the native's cell pitch for a screen of the same size;
* the **back-end selector** is the forced flag of the acceptance
  criteria: `auto`, `webgpu`, `webgl2` or `canvas2d`. A forced back end
  that the browser does not have falls back to `auto`, and the status
  line says so;
* the **pause** button stops the parade;
* the **file input** takes a `.ducks` package (the native
  `ducks export` output) or a bare `manifest.json` (the native
  `ducks config export` output);
* the status line shows the back end, the grid, the run state and the
  number of drawn assets;
* the diagnostic box (`<pre id="diag">`) collects every `error`,
  `unhandledrejection` and `console.error`, so a headless run can read
  the failures out of the DOM.

## The mouse

The browser has a pointer, and this is where the web build is **better**
than the native one:

* a **click** on a duck makes it **jump** — it keeps marching;
* a **shift-click** **holds** it, and the ducks behind it.

The chain is the core's: page coordinate → grid cell → train slot
(`Scene::duck_at`) → reaction (`Scene::react`).

## The exported API

```rust
#[wasm_bindgen(start)] pub fn start();
#[wasm_bindgen]        pub fn backend_name() -> String;
#[wasm_bindgen]        pub fn set_backend(name: &str) -> Result<(), String>;
#[wasm_bindgen]        pub fn set_paused(paused: bool);
#[wasm_bindgen]        pub fn poke_duck(x: f32, y: f32, shift: bool) -> bool;
#[wasm_bindgen]        pub fn apply_manifest(json: &str) -> Result<String, String>;
#[wasm_bindgen]        pub fn load_package(bytes: &[u8]) -> Result<String, String>;
#[wasm_bindgen]        pub fn status_line() -> String;
```

`poke_duck` takes **page** coordinates (as `MouseEvent.client_x` gives
them). The canvas' own clicks are handled inside the wasm
(`src/input.rs`), so the page does not have to.

## The core APIs this crate uses

* `ducks_core::scene::Scene::build` — the **injectable** constructor:
  the loader is a parameter, so the core never reaches for a folder and
  the page's in-memory store answers it;
* `ducks_core::scene::Scene::resize` — a new grid, the same simulation;
* `ducks_core::blit::{FrameBuffer, blit_scene}` — the one frame
  composition (the sky, the parallax layers, the parade at its sub-cell
  offset, the grass at offset zero);
* `ducks_core::font::Font::new` — the glyph rasterizer at the cell
  pitch;
* `ducks_core::config::Config::{default, grid_for_monitor}` — the
  configuration and the row-budget grid;
* `ducks_core::reaction::ReactionKind` — `Jump` and `Stop`;
* `ducks_core::manifest::{Manifest, MANIFEST_ENTRY}` — the schema, its
  validation, its config and its assignment table;
* `ducks_core::package::ZipArchive::{parse, read}` — the store-only ZIP
  reader (the read half of the core's package support; the write half
  writes to a folder, and a browser has no folder);
* `ducks_core::drawn::{DrawnDoc, DrawnSprite, DrawnKind}` — the drawn
  documents and their runtime form.

## The smoke test's oracle

`tools/pngcheck.py` reads a PNG with the standard library only and
prints its size, its mean colour, its standard deviation, its number of
distinct colours and whether it is **uniformly black**. It exits 1 when
the image is uniformly black, so "a wasm build that compiles but renders
a black rectangle" fails:

```bash
python3 crates/ducks-web/tools/pngcheck.py shot.png
```

## What the web build does NOT have

The feature table of the roadmap §2, in short: there is no global
hotkey, no browser minimize (the web **is** the browser), no folder
watcher (there is a file input), no autostart and no editor (the editor
stays native; the **art** travels through a manifest or a package).
Sound is native-first: the core still emits its audio events, but the
web has no audio device yet.

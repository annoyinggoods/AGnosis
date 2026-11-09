//! User-drawn assets ("draw your own"): the document format, the editor
//! palette and tool list, the canvas geometry and the on-disk storage.
//!
//! Stage 1 of `REVIEW/ROADMAP_DRAW_EDITOR.md` (§3, §6 Stage 1). This module
//! is deliberately **pure data plus file I/O** — no UI, no scene, no hotkey —
//! so the whole format can be tested without a display.
//!
//! Why the format looks the way it does (roadmap §3.3–§3.7):
//!
//! * The canvas of a **sprite** is square, with its side equal to the
//!   element's frame width (decision D1): 150 cells for the big duck, 75
//!   for the small one. A **balloon**'s canvas is the disc's own cell
//!   grid (`2rx+1` by `2ry+1` cells, 109 x 73 at the default radius 36):
//!   the grid cell is taller than wide, so a 3:2 cell grid is a circle in
//!   pixels, and the art maps 1:1 onto the runtime disc. The square
//!   balloon canvas this replaced stretched every drawn balloon 1.5x
//!   horizontally (audit 2026-10-20 § 3.18).
//! * Only the frame rectangle at the canvas origin is drawable, and that
//!   rectangle **is** the runtime frame (decision D2). The document stores
//!   the frame, never the canvas, so a drawn sprite is a drop-in for the
//!   built-in art and the parade spacing never changes.
//! * The art keeps its **per-cell colour** (decision D16). The built-in
//!   ducks' single `DUCK_COLOR` does not apply to user art.
//! * Drawn assets live in their own folders (decision D5): the balloon folder
//!   is watched and its `.ans` path is single-coloured, so user art must not
//!   go there.
//! * Pure black is not in the palette (decision D3): the canvas background
//!   is black, so a black cell would read as an empty cell.

// Stage 1 of the roadmap lands the module *before* its consumers: the editor
// (Stage 4+) and the swap feature (Stage 8) are what call this API, so the
// whole module is "never used" from the binary's point of view until then.
// The blanket allow that covered this file is gone as of Stage 8, and the
// per-item allows that replaced it are gone as of Stage 9: the editor now
// consumes the whole painting API (the palette and tool list, the canvas
// geometry, the crop-on-save and the save path), so no item in this file
// carries a `dead_code` allow any more. A genuinely dead item shows up in
// the build again — that is the point of the sweep.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::balloon::AsciiSprite;
use crate::grid::Grid;

/// The 8 basic colours offered by the editor's colour panel.
/// Index 0 (white) is the default.
///
/// Pure black is deliberately NOT in the set: the canvas background is
/// black, so a black cell would look like an empty cell and read as a bug.
/// Grey covers the "muted" slot that black would otherwise fill.
pub const PALETTE: [[u8; 3]; 8] = [
    [255, 255, 255], // 0 WHITE   (default)
    [255, 80, 80],   // 1 RED
    [80, 255, 80],   // 2 GREEN
    [255, 255, 80],  // 3 YELLOW
    [80, 80, 255],   // 4 BLUE
    [255, 80, 255],  // 5 MAGENTA
    [80, 255, 255],  // 6 CYAN
    [128, 128, 128], // 7 GREY
];

/// Display names, used by the editor's status line.
pub const PALETTE_NAMES: [&str; 8] = [
    "WHITE", "RED", "GREEN", "YELLOW", "BLUE", "MAGENTA", "CYAN", "GREY",
];

/// The vertical tool list on the right of the editor canvas.
/// Index 1 (`'x'`) is the default tool.
pub const TOOLS: [char; 19] = [
    'X', 'x', '.', '/', '\\', 'o', 'O', '`', '_', '-', '~', '|', '+', '*', ',', '<', '>', '^', '#',
];

/// Default tool index.
pub const DEFAULT_TOOL: usize = 1;
/// Default colour index.
pub const DEFAULT_COLOR: usize = 0;

/// The document version this build writes and understands (roadmap §3.5).
pub const DOC_VERSION: u32 = 1;

/// Longest accepted asset name (roadmap §3.7).
pub const MAX_NAME_LEN: usize = 24;

/// Largest document this build reads (roadmap §9: a huge document must not
/// blow up memory). A four-frame big sprite serialises to well under this.
pub const MAX_DOC_BYTES: u64 = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Document format (roadmap §3.5)
// ---------------------------------------------------------------------------

/// What kind of asset a document holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrawnKind {
    /// A walking sprite that replaces a duck in the parade.
    #[default]
    Sprite,
    /// A balloon that replaces an image balloon in the parade.
    Balloon,
    /// A grass **tile** that replaces the built-in grass band.
    ///
    /// A tile is not a frame: its dimensions are the user's, within the
    /// caps of [`GRASS_TILE_MAX_ROWS`] and [`GRASS_TILE_MAX_COLS`]
    /// (roadmap Extension 1 §10.1).
    Grass,
    /// A **decoration** that is drawn over a duck's body: a hat, a
    /// scarf, a bowtie (roadmap Extension 2 §4).
    ///
    /// A decoration is a full-size frame — the duck's own frame — so
    /// there is no anchoring problem: the user draws the hat exactly
    /// where they want it, on the duck's own grid.
    Decoration,
}

/// The grass tile's row cap: a tile is a band, not a picture.
pub const GRASS_TILE_MAX_ROWS: usize = 4;

/// The grass tile's column cap.
pub const GRASS_TILE_MAX_COLS: usize = 200;

impl DrawnKind {
    /// The file-name prefix of a generated asset name (`sprite-1`,
    /// `balloon-1`).
    ///
    /// The **folder** names live in the platform layer (`sprites`,
    /// `balloons`) — one owner for every location, so the two can never
    /// drift apart.
    pub fn prefix(&self) -> &'static str {
        match self {
            DrawnKind::Sprite => "sprite",
            DrawnKind::Balloon => "balloon",
            DrawnKind::Grass => "grass",
            DrawnKind::Decoration => "decoration",
        }
    }
}

impl std::fmt::Display for DrawnKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.prefix())
    }
}

/// Which built-in duck size a drawn sprite imitates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DrawnSize {
    /// The big duck: 150 x 70 cells.
    Big,
    /// The small duck: 75 x 35 cells.
    Small,
}

impl std::fmt::Display for DrawnSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DrawnSize::Big => "big",
            DrawnSize::Small => "small",
        })
    }
}

/// One animation frame: the characters and the per-cell colours of a
/// rectangle of cells. The shape is the existing `AsciiSprite` body, so
/// `AsciiSprite::from_cells` can build the runtime frame with no new cell
/// machinery (decision D6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameDoc {
    /// Frame width in cells (150 / 75 / the balloon disc's width).
    pub cols: usize,
    /// Frame height in cells (70 / 35 / the balloon disc's height).
    pub rows: usize,
    /// One character per cell, row-major; `' '` is transparent.
    pub chars: Vec<char>,
    /// One RGB colour per cell, row-major.
    pub colors: Vec<[u8; 3]>,
}

/// A drawn asset on disk: one JSON file per asset.
///
/// `#[serde(default)]` keeps a hand-written or older document loadable: a
/// missing field falls back to [`DrawnDoc::default`], whose version is
/// [`DOC_VERSION`] — so "missing" never reads as "version 0".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawnDoc {
    /// Format version; a document from the future is refused, never misread.
    pub version: u32,
    /// Sprite or balloon.
    pub kind: DrawnKind,
    /// The duck size of a sprite; `None` for a balloon.
    pub size: Option<DrawnSize>,
    /// The animation frames, in order.
    pub frames: Vec<FrameDoc>,
    /// The optional **reaction frame**: the pose the duck holds while it
    /// reacts (roadmap Extension 3 §5.3).
    ///
    /// It is a *separate* frame, not another walk frame: the walk cycle
    /// must not change its length, or the art's own frame count would
    /// change the parade's rhythm.
    ///
    /// The field is optional and the document is `#[serde(default)]`, so
    /// an older document without it still loads — and the version is NOT
    /// bumped, because the version check is a strict equality and a bump
    /// would refuse every document the user already has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reaction: Option<FrameDoc>,
}

impl Default for DrawnDoc {
    fn default() -> Self {
        Self {
            version: DOC_VERSION,
            kind: DrawnKind::Sprite,
            size: None,
            frames: Vec::new(),
            reaction: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime type (roadmap §3.6)
// ---------------------------------------------------------------------------

/// A loaded user-drawn asset: the runtime frames of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawnSprite {
    /// Sprite or balloon.
    pub kind: DrawnKind,
    /// The duck size of a sprite; `None` for a balloon.
    pub size: Option<DrawnSize>,
    /// One `AsciiSprite` per animation frame. A sprite frame is the duck
    /// frame (150x70 / 75x35); a balloon frame is the authoring disc.
    pub frames: Vec<AsciiSprite>,
    /// The optional **reaction frame**: the pose the duck holds while it
    /// reacts (roadmap Extension 3 §5.3).
    ///
    /// The built-in art cannot carry one (it is generated and drift
    /// guarded), so the built-in duck gets the squash instead; a drawn
    /// duck may bring its own face.
    pub reaction: Option<AsciiSprite>,
}

impl DrawnSprite {
    /// Build the runtime asset from a document.
    ///
    /// Every dimension is validated before a single cell is allocated: the
    /// version must be the one this build understands, the frame shape must
    /// match the kind (the built-in duck frame for a sprite, the authoring
    /// disc for a balloon), and `chars`/`colors` must both hold
    /// `cols * rows` entries.
    /// A rejected document yields an error, never a panic — release builds
    /// abort on panic (audit robustness).
    pub fn from_doc(doc: &DrawnDoc) -> Result<DrawnSprite, DrawnError> {
        if doc.version != DOC_VERSION {
            return Err(DrawnError::UnsupportedVersion {
                version: doc.version,
                supported: DOC_VERSION,
            });
        }
        let size = match doc.kind {
            DrawnKind::Sprite => Some(doc.size.ok_or_else(|| DrawnError::BadDimensions {
                detail: "a sprite document needs a size (big or small)".to_string(),
            })?),
            // A balloon has no size; a stray value in the file is normalised
            // away rather than rejected, so a hand-edited document still
            // loads. A grass tile has no size either — its dimensions are
            // the user's, inside the tile's caps.
            DrawnKind::Balloon | DrawnKind::Grass => None,
            // A decoration is drawn over a duck, so it carries the duck's
            // size: its frame is the big frame or the small one.
            DrawnKind::Decoration => Some(doc.size.ok_or_else(|| DrawnError::BadDimensions {
                detail: "a decoration document needs a size (big or small)".to_string(),
            })?),
        };
        let shape = frame_shape(doc.kind, size)?;
        if doc.frames.is_empty() {
            return Err(DrawnError::BadDimensions {
                detail: "the document has no frames".to_string(),
            });
        }
        let mut frames = Vec::with_capacity(doc.frames.len());
        for (index, frame) in doc.frames.iter().enumerate() {
            frames.push(build_frame(frame, shape, &format!("frame {index}"))?);
        }
        let reaction = match &doc.reaction {
            Some(frame) => Some(build_frame(frame, shape, "the reaction frame")?),
            None => None,
        };
        Ok(DrawnSprite {
            kind: doc.kind,
            size,
            frames,
            reaction,
        })
    }

    /// The document this asset is stored as.
    pub fn to_doc(&self) -> DrawnDoc {
        DrawnDoc {
            version: DOC_VERSION,
            kind: self.kind,
            size: self.size,
            frames: self.frames.iter().map(frame_to_doc).collect(),
            reaction: self.reaction.as_ref().map(frame_to_doc),
        }
    }

    /// Number of animation frames (at least one, so a modulo never divides
    /// by zero).
    pub fn frame_count(&self) -> usize {
        self.frames.len().max(1)
    }

    /// The frame at `index`, wrapping over the frame count.
    pub fn frame(&self, index: usize) -> Option<&AsciiSprite> {
        if self.frames.is_empty() {
            return None;
        }
        self.frames.get(index % self.frames.len())
    }

    /// The reaction frame, if the asset carries one.
    ///
    /// `None` means "no own face": the built-in squash is used instead.
    pub fn reaction_frame(&self) -> Option<&AsciiSprite> {
        self.reaction.as_ref()
    }

    /// Stamp one frame onto the grid with **per-cell colour**.
    ///
    /// Off-grid cells are clipped, never wrapped: a duck entering or leaving
    /// the screen must not reappear on the opposite edge (the rule the
    /// built-in sprites follow in [`crate::sprites`]). The built-in ducks'
    /// single `DUCK_COLOR` does not apply here — the user's per-cell colours
    /// are what the art carries.
    pub fn draw(&self, grid: &mut Grid, col: i64, row: i64, frame: usize) {
        let Some(sprite) = self.frame(frame) else {
            return;
        };
        stamp_drawn_sprite(grid, sprite, col, row);
    }

    /// Draw the frame compressed vertically by `squash` (0.0 = normal,
    /// 1.0 = fully squashed). The duck's **feet stay on the ground row**:
    /// the frame's bottom row keeps its place and the frame's top comes
    /// down.
    ///
    /// A `squash` of `0.0` draws **exactly** what [`DrawnSprite::draw`]
    /// draws; that is the invariant the landing curve leans on.
    pub fn draw_squashed(&self, grid: &mut Grid, col: i64, row: i64, frame: usize, squash: f32) {
        let Some(sprite) = self.frame(frame) else {
            return;
        };
        stamp_drawn_sprite_squashed(grid, sprite, col, row, squash);
    }

    /// Draw the **reaction frame**, if the asset carries one.
    ///
    /// Returns `true` when the frame was drawn: the caller falls back to
    /// the walking frame (squashed) when the asset brings no face of its
    /// own.
    pub fn draw_reaction(&self, grid: &mut Grid, col: i64, row: i64) -> bool {
        let Some(frame) = self.reaction_frame() else {
            return false;
        };
        stamp_drawn_sprite(grid, frame, col, row);
        true
    }
}

// ---------------------------------------------------------------------------
// Canvas geometry (roadmap §3.3, §3.4)
// ---------------------------------------------------------------------------

/// The side of the square drawing canvas of a sprite: the sprite's frame
/// width (150 big, 75 small).
pub fn sprite_canvas_side(size: DrawnSize) -> usize {
    match size {
        DrawnSize::Big => crate::sprites::BIG_DUCK_COLS,
        DrawnSize::Small => crate::sprites::SMALL_DUCK_COLS,
    }
}

/// The runtime frame of a sprite: the built-in duck frame (150x70 big,
/// 75x35 small). This is the **frame**, not the canvas.
pub fn sprite_frame_size(size: DrawnSize) -> (usize, usize) {
    match size {
        DrawnSize::Big => (crate::sprites::BIG_DUCK_COLS, crate::sprites::BIG_DUCK_ROWS),
        DrawnSize::Small => (
            crate::sprites::SMALL_DUCK_COLS,
            crate::sprites::SMALL_DUCK_ROWS,
        ),
    }
}

/// The drawable height inside the square sprite canvas: the frame height
/// (70 big, 35 small). The rows below it are inactive.
pub fn sprite_active_rows(size: DrawnSize) -> usize {
    sprite_frame_size(size).1
}

// The frame of a balloon's drawing canvas is the **disc's own cell grid**,
// `2rx + 1` by `2ry + 1` cells (109 x 73 at the default radius 36).
//
// The frame is not square: the grid cell is taller than wide, so a disc
// that is 3:2 in cells is a circle in pixels. A square canvas would be a
// tall ellipse in pixels, and the runtime stretched every balloon drawn
// in one 1.5x horizontally (audit 2026-10-20 § 3.18).
//
// The geometry itself lives with the disc rasterizer in
// [`crate::balloon`] ([`crate::balloon::balloon_authoring_geometry`]),
// and so does the active-cell mask
// ([`crate::balloon::balloon_authoring_mask`]): one owner for the shape
// of a balloon, so the editor's clickable area and the rendered balloon
// are the same shape (decision D14).

/// Crop the square canvas down to the frame the document stores.
///
/// For a sprite the rows below the active region are dropped (150x150 ->
/// 150x70). For a balloon the canvas **is** the frame, so it comes back
/// unchanged. This is what the editor calls before saving (roadmap §3.4).
pub fn crop_to_active(canvas: &AsciiSprite, kind: DrawnKind, size: DrawnSize) -> AsciiSprite {
    match kind {
        DrawnKind::Balloon | DrawnKind::Grass => canvas.clone(),
        // A decoration is authored on the duck's frame, so it is cropped
        // exactly like the duck it is drawn over.
        DrawnKind::Sprite | DrawnKind::Decoration => {
            crop_sprite_canvas(canvas, sprite_active_rows(size))
        }
    }
}

/// Drop the rows below the active region of a sprite canvas.
fn crop_sprite_canvas(canvas: &AsciiSprite, active_rows: usize) -> AsciiSprite {
    let cols = canvas.cols();
    let rows = active_rows.min(canvas.rows());
    let keep = cols.saturating_mul(rows);
    let chars: Vec<char> = canvas.chars().iter().take(keep).copied().collect();
    let colors: Vec<[u8; 3]> = canvas.colors().iter().take(keep).copied().collect();
    AsciiSprite::from_cells(cols, rows, chars, colors)
        .unwrap_or_else(|| AsciiSprite::new(cols, rows))
}

// ---------------------------------------------------------------------------
// Slot mapping (roadmap §3.8)
// ---------------------------------------------------------------------------

/// Train slot of the user's "big duck n" (`n` in 1..=2).
///
/// The mapping is derived from the parade's own tables, so it cannot drift
/// from the train layout: the big ducks are the lead slots
/// ([`crate::balloon::BIG_DUCK_SLOTS`]).
pub fn big_slot(n: usize) -> Option<usize> {
    let index = n.checked_sub(1)?;
    crate::balloon::BIG_DUCK_SLOTS.get(index).copied()
}

/// Train slot of the user's "small duck n" (`n` in 1..=9).
///
/// The small ducks are the train slots behind the big ones, so the mapping
/// starts at [`crate::parade::BIG_SLOTS`] and ends at the last slot of the
/// train ([`crate::parade::DUCK_COUNT`] - 1).
pub fn small_slot(n: usize) -> Option<usize> {
    let index = n.checked_sub(1)?;
    if index >= crate::parade::DUCK_COUNT - crate::parade::BIG_SLOTS {
        return None;
    }
    Some(crate::parade::BIG_SLOTS + index)
}

/// Ring slot of the user's "balloon n" (`n` in 1..=5).
///
/// One balloon per holder; the holders are the ducks listed in
/// [`crate::balloon::BALLOON_HOLDER_DUCK`].
pub fn balloon_slot(n: usize) -> Option<usize> {
    let index = n.checked_sub(1)?;
    if index >= crate::balloon::BALLOON_HOLDER_DUCK.len() {
        return None;
    }
    Some(index)
}

// ---------------------------------------------------------------------------
// Storage (roadmap §3.7)
// ---------------------------------------------------------------------------

/// Everything that can go wrong while storing or reading a drawn asset.
#[derive(Debug, thiserror::Error)]
pub enum DrawnError {
    /// The name is not a legal asset name (no path traversal, ever).
    #[error(
        "illegal asset name {name:?} (use a-z, 0-9, '-' or '_', max {MAX_NAME_LEN} characters)"
    )]
    InvalidName { name: String },
    /// A file could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A file is not a valid document.
    #[error("invalid json at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The document was written by a newer build.
    #[error(
        "unsupported drawn-document version {version} (this build supports version {supported})"
    )]
    UnsupportedVersion { version: u32, supported: u32 },
    /// The document's cells do not match its declared frame.
    #[error("bad drawn-document dimensions: {detail}")]
    BadDimensions { detail: String },
    /// No asset of that name exists.
    #[error("no drawn asset named {name:?}")]
    NotFound { name: String },
    /// The document is bigger than this build reads.
    #[error("drawn document at {path} is {bytes} bytes (the limit is {MAX_DOC_BYTES})")]
    TooLarge { path: PathBuf, bytes: u64 },
    /// The folder and the asset disagree about the kind.
    #[error("expected a {expected} asset but found a {actual} one")]
    KindMismatch {
        expected: DrawnKind,
        actual: DrawnKind,
    },
    /// The platform config folder could not be resolved.
    #[error("could not resolve the drawn-asset folder: {source}")]
    Path {
        #[source]
        source: crate::path_error::PathError,
    },
}

/// Save an asset into `<dir>/<name>.json`.
///
/// `dir` is the folder of that kind (`<config>/drawn/sprites` or
/// `<config>/drawn/balloons`). The explicit folder is what the tests use:
/// they point it at a temp dir and never touch the user's real config
/// folder.
///
/// An asset that is saved **over an existing one** keeps a best-effort
/// `<name>.json.bak` of the old version (audit 2026-10-23 §F-061): the
/// write itself is atomic, so a crash never truncates the file, but an
/// ordinary re-save used to destroy the previous version with no way
/// back. The backup is best effort — a failure to copy is worth a trace
/// line, not a failed save.
pub fn save_to(
    dir: &Path,
    sprite: &DrawnSprite,
    kind: DrawnKind,
    name: &str,
) -> Result<PathBuf, DrawnError> {
    validate_name(name)?;
    if sprite.kind != kind {
        return Err(DrawnError::KindMismatch {
            expected: kind,
            actual: sprite.kind,
        });
    }
    validate_runtime(sprite)?;
    std::fs::create_dir_all(dir).map_err(|source| DrawnError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(format!("{name}.json"));
    let text =
        serde_json::to_string_pretty(&sprite.to_doc()).map_err(|source| DrawnError::Json {
            path: path.clone(),
            source,
        })?;
    keep_backup(&path);
    // WHY atomic: `std::fs::write` truncates the target first, so a
    // crash (or the release profile's `panic = "abort"`) mid-write left a
    // truncated `<name>.json` behind — and a truncated document fails to
    // parse, so the user's drawing was simply gone (audit 2026-10-21
    // §F11).
    write_atomic(&path, &text)?;
    Ok(path)
}

/// Copy an existing asset to its `.bak` slot, best effort.
///
/// The `.bak` sits beside the asset and carries a different extension,
/// so the listings (`list_in` looks for `.json` stems only) never offer
/// it as an asset.
fn keep_backup(path: &Path) {
    if !path.exists() {
        return;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "asset.json".to_string());
    let backup = path.with_file_name(format!("{name}.bak"));
    if let Err(e) = std::fs::copy(path, &backup) {
        crate::trace::warn(&format!(
            "drawn: could not write the backup {}: {e}",
            backup.display()
        ));
    }
}

/// Load an asset from `<dir>/<name>.json`.
pub fn load_from(dir: &Path, kind: DrawnKind, name: &str) -> Result<DrawnSprite, DrawnError> {
    validate_name(name)?;
    let path = dir.join(format!("{name}.json"));
    let meta = std::fs::metadata(&path).map_err(|source| io_or_missing(&path, name, source))?;
    if meta.len() > MAX_DOC_BYTES {
        return Err(DrawnError::TooLarge {
            path,
            bytes: meta.len(),
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|source| DrawnError::Io {
        path: path.clone(),
        source,
    })?;
    let doc: DrawnDoc = serde_json::from_str(&text).map_err(|source| DrawnError::Json {
        path: path.clone(),
        source,
    })?;
    if doc.kind != kind {
        return Err(DrawnError::KindMismatch {
            expected: kind,
            actual: doc.kind,
        });
    }
    DrawnSprite::from_doc(&doc)
}

/// The names of the assets in `dir`, sorted.
pub fn list_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| asset_stem(&entry.path()))
        .collect();
    names.sort();
    names
}

/// One asset in a listing: its name and the duck size it imitates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetEntry {
    /// The asset's name — the file's stem.
    pub name: String,
    /// The duck size the asset imitates: `None` for a balloon or a grass
    /// tile, and for a platform that does not report the sizes.
    pub size: Option<DrawnSize>,
}

/// The assets of a folder, with their size class.
///
/// WHY the size: the assign panel must offer a big slot **only** the big
/// sprites — a small sprite in a big slot is drawn in the top-left
/// corner of the big duck's box, which is a broken picture (audit
/// 2026-10-21 §E1). The size is in each document, so the listing reads
/// every file's header.
///
/// A file that cannot be read is **skipped**: a broken document must not
/// hide the other assets of the folder, and the panel cannot offer an
/// asset it cannot load anyway. A file **bigger than
/// [`MAX_DOC_BYTES`]** is skipped the same way — the listing used to
/// parse every document whole with no cap, so one oversized stray file
/// made the panel read hundreds of megabytes (audit 2026-10-23 §F-054;
/// the load path has enforced the cap since §F11).
pub fn list_with_sizes_in(dir: &Path) -> Vec<AssetEntry> {
    list_in(dir)
        .into_iter()
        .filter_map(|name| asset_entry_in(dir, name))
        .collect()
}

/// One asset of the folder, or `None` when its document cannot be read.
///
/// The size is checked **before** the file is read, so an oversized file
/// costs one `metadata` call, not a read (audit 2026-10-23 §F-054).
fn asset_entry_in(dir: &Path, name: String) -> Option<AssetEntry> {
    let path = dir.join(format!("{name}.json"));
    let Ok(meta) = std::fs::metadata(&path) else {
        return None;
    };
    if meta.len() > MAX_DOC_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let doc: DrawnDoc = serde_json::from_str(&text).ok()?;
    Some(AssetEntry {
        name,
        size: doc.size,
    })
}

// ---------------------------------------------------------------------------
// Drafts (roadmap Extension 2 §2.2)
// ---------------------------------------------------------------------------

/// The version this build writes and understands for a draft.
pub const DRAFT_VERSION: u32 = 1;

/// The suffix of a draft file.
pub const DRAFT_SUFFIX: &str = ".draft.json";

/// A draft: the drawing as it stands, saved between sessions.
///
/// A draft is **not** an asset: it lives in its own folder (the
/// platform's `drafts` folder), so the scene and `agnosis list-drawn`
/// can never pick a half-finished drawing up. The payload is the same
/// [`DrawnDoc`] the assets use, plus the two fields the document format
/// has no place for: how many frames the asset will have, and which
/// frame was being drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DraftDoc {
    /// Format version; a draft from the future is refused, never misread.
    pub version: u32,
    /// The kind of asset being drawn.
    pub kind: DrawnKind,
    /// The duck size of a sprite; `None` for a balloon.
    pub size: Option<DrawnSize>,
    /// The frames that are already finished, cropped.
    pub frames: Vec<FrameDoc>,
    /// The canvas as it stands — **not** cropped, so the drawing can be
    /// resumed on the very same grid.
    pub canvas: FrameDoc,
    /// How many frames the asset will have (1..=4).
    pub frames_wanted: usize,
    /// The frame that was being drawn, 0-based.
    pub frame_index: usize,
    /// The asset name the draft belongs to.
    pub name: String,
}

impl Default for DraftDoc {
    fn default() -> Self {
        Self {
            version: DRAFT_VERSION,
            kind: DrawnKind::Sprite,
            size: None,
            frames: Vec::new(),
            canvas: FrameDoc {
                cols: 0,
                rows: 0,
                chars: Vec::new(),
                colors: Vec::new(),
            },
            frames_wanted: 1,
            frame_index: 0,
            name: String::new(),
        }
    }
}

/// One draft found in the drafts folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftEntry {
    /// The asset name the draft belongs to.
    pub name: String,
    /// The draft file.
    pub path: PathBuf,
    /// When the file was last written, in seconds since the epoch.
    ///
    /// The newest draft is the one the editor offers: a stale draft from
    /// last week must not win over yesterday's work.
    pub modified: u64,
}

/// The file name of the draft of `kind` and `name`.
///
/// The kind is part of the name, so a draft of a sprite and a draft of a
/// balloon never share a file.
pub fn draft_file_name(kind: DrawnKind, name: &str) -> String {
    format!("{}-{name}{DRAFT_SUFFIX}", kind.prefix())
}

/// The name of the asset a draft file belongs to, or `None` when the file
/// is not a draft of `kind`.
fn draft_asset_name(kind: DrawnKind, file_name: &str) -> Option<String> {
    let prefix = format!("{}-", kind.prefix());
    let rest = file_name.strip_prefix(&prefix)?;
    let name = rest.strip_suffix(DRAFT_SUFFIX)?;
    if name.is_empty() {
        return None;
    }
    Some(name.to_string())
}

/// Write a draft into `<dir>/<kind>-<name>.draft.json`.
///
/// The write is **atomic** (temp file + rename), the same discipline the
/// config writer uses: a draft that is half-written is worse than no
/// draft, because the editor would offer a corrupt drawing.
///
/// The name is validated first, so a name can never escape the drafts
/// folder.
pub fn save_draft_to(
    dir: &Path,
    doc: &DraftDoc,
    kind: DrawnKind,
    name: &str,
) -> Result<PathBuf, DrawnError> {
    validate_name(name)?;
    std::fs::create_dir_all(dir).map_err(|source| DrawnError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(draft_file_name(kind, name));
    let text = serde_json::to_string_pretty(doc).map_err(|source| DrawnError::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(&path, &text)?;
    Ok(path)
}

/// Read the draft of `kind` and `name` from `dir`.
///
/// `Ok(None)` when there is no such draft: a missing draft is not an
/// error, it is simply "nothing was left behind".
pub fn load_draft_from(
    dir: &Path,
    kind: DrawnKind,
    name: &str,
) -> Result<Option<DraftDoc>, DrawnError> {
    validate_name(name)?;
    let path = dir.join(draft_file_name(kind, name));
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(DrawnError::Io { path, source }),
    };
    let doc: DraftDoc =
        serde_json::from_str(&text).map_err(|source| DrawnError::Json { path, source })?;
    if doc.version != DRAFT_VERSION {
        return Err(DrawnError::UnsupportedVersion {
            version: doc.version,
            supported: DRAFT_VERSION,
        });
    }
    Ok(Some(doc))
}

/// Delete the draft of `kind` and `name` from `dir`.
///
/// Returns `true` when a draft was there. A missing draft is not an
/// error — the editor calls this on every save, and a save without a
/// draft is the normal case.
pub fn delete_draft_from(dir: &Path, kind: DrawnKind, name: &str) -> Result<bool, DrawnError> {
    validate_name(name)?;
    let path = dir.join(draft_file_name(kind, name));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(DrawnError::Io { path, source }),
    }
}

/// The drafts of `kind` in `dir`, newest first.
///
/// The folder is read **once**: the editor offers the newest draft when
/// it opens, and it must not scan the folder on every frame.
pub fn list_drafts(dir: &Path, kind: DrawnKind) -> Vec<DraftEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<DraftEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let file_name = path.file_name()?.to_str()?.to_string();
            let name = draft_asset_name(kind, &file_name)?;
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_secs())
                .unwrap_or(0);
            Some(DraftEntry {
                name,
                path,
                modified,
            })
        })
        .collect();
    // Newest first; the name breaks a tie, so the order is total and the
    // offer is never ambiguous.
    found.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.name.cmp(&b.name))
    });
    found
}

/// The document frame of a runtime frame.
pub fn frame_doc(sprite: &AsciiSprite) -> FrameDoc {
    frame_to_doc(sprite)
}

/// Write `text` to `path` atomically.
///
/// WHY atomic: a draft is written while the editor is running, and a
/// crash (or the release profile's `panic = "abort"`) must never leave a
/// half-written draft behind — the editor would then offer a corrupt
/// drawing. Writing a temp file and renaming it over the target is
/// atomic on every platform the app supports.
fn write_atomic(path: &Path, text: &str) -> Result<(), DrawnError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| DrawnError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "draft.json".to_string());
    let temp = match path.parent() {
        Some(parent) => parent.join(format!("{file_name}.{}.tmp", std::process::id())),
        None => std::path::PathBuf::from(format!("{file_name}.{}.tmp", std::process::id())),
    };
    std::fs::write(&temp, text).map_err(|source| DrawnError::Io {
        path: temp.clone(),
        source,
    })?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(DrawnError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// The first free generated name of `kind` in `dir`.
///
/// The scan is bounded: with `n` taken names there are `n + 2` candidates,
/// so one of them is always free.
pub fn next_free_name_in(dir: &Path, kind: DrawnKind) -> String {
    let taken = list_in(dir);
    (1..=taken.len() + 2)
        .map(|index| format!("{}-{index}", kind.prefix()))
        .find(|candidate| !taken.iter().any(|name| name == candidate))
        .unwrap_or_else(|| format!("{}-1", kind.prefix()))
}

/// The file stem of a `.json` asset, or `None` for anything else.
fn asset_stem(path: &Path) -> Option<String> {
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return None;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

/// Reject a name that could escape the asset folder.
///
/// The legal set is `a-z`, `0-9`, `-` and `_`; a path separator, a dot and
/// anything else is refused, so `../escape` can never become a path.
///
/// Public because the CLI's `assign` must refuse an illegal name at the
/// user edge, before it ever reaches the config file.
pub fn validate_name(name: &str) -> Result<(), DrawnError> {
    let legal_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    let legal = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name.chars().all(legal_char)
        && !name.contains('/')
        && !name.contains('\\')
        && name != "..";
    if !legal {
        return Err(DrawnError::InvalidName {
            name: name.to_string(),
        });
    }
    Ok(())
}

/// Turn a missing file into "not found" and everything else into an I/O
/// error.
fn io_or_missing(path: &Path, name: &str, source: std::io::Error) -> DrawnError {
    if source.kind() == std::io::ErrorKind::NotFound {
        return DrawnError::NotFound {
            name: name.to_string(),
        };
    }
    DrawnError::Io {
        path: path.to_path_buf(),
        source,
    }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// The frame shape a document of this kind must have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameShape {
    /// Exactly these dimensions — a sprite frame is the built-in duck frame.
    Exact(usize, usize),
    /// An **authoring disc** — a balloon frame is the disc's own cell grid
    /// (`2rx+1` by `2ry+1` cells with `rx = 1.5 * ry`), and its size
    /// follows the configured balloon radius (roadmap §3.3), so only the
    /// shape is fixed here, not the size.
    ///
    /// The old square frame is refused: it is the shape that stretched
    /// every drawn balloon 1.5x horizontally (audit 2026-10-20 § 3.18).
    AuthoringDisc,
    /// A **tile**: the dimensions are the user's, inside the tile's caps.
    /// A grass band is 2-4 rows and a few dozen columns, so the cap is a
    /// shape check and not an exact size.
    Tile,
}

/// The frame shape required by `kind` and `size`.
fn frame_shape(kind: DrawnKind, size: Option<DrawnSize>) -> Result<FrameShape, DrawnError> {
    match kind {
        DrawnKind::Sprite => {
            let size = size.ok_or_else(|| DrawnError::BadDimensions {
                detail: "a sprite document needs a size (big or small)".to_string(),
            })?;
            let (cols, rows) = sprite_frame_size(size);
            Ok(FrameShape::Exact(cols, rows))
        }
        DrawnKind::Balloon => Ok(FrameShape::AuthoringDisc),
        DrawnKind::Grass => Ok(FrameShape::Tile),
        // A decoration's frame is the duck's frame: it is drawn over the
        // body, cell for cell.
        DrawnKind::Decoration => {
            let size = size.ok_or_else(|| DrawnError::BadDimensions {
                detail: "a decoration document needs a size (big or small)".to_string(),
            })?;
            let (cols, rows) = sprite_frame_size(size);
            Ok(FrameShape::Exact(cols, rows))
        }
    }
}

/// Check that a frame's dimensions match the shape its kind requires.
fn check_frame_shape(
    shape: FrameShape,
    cols: usize,
    rows: usize,
    what: &str,
) -> Result<(), DrawnError> {
    let (ok, expected) = match shape {
        FrameShape::Exact(want_cols, want_rows) => (
            cols == want_cols && rows == want_rows,
            format!("{want_cols}x{want_rows}"),
        ),
        FrameShape::AuthoringDisc => (
            crate::balloon::is_authoring_disc(cols, rows),
            "an authoring disc frame (2rx+1 by 2ry+1 cells, rx = 1.5 ry)".to_string(),
        ),
        FrameShape::Tile => (
            (1..=GRASS_TILE_MAX_ROWS).contains(&rows) && (1..=GRASS_TILE_MAX_COLS).contains(&cols),
            format!(
                "a tile of 1..={GRASS_TILE_MAX_ROWS} rows and 1..={GRASS_TILE_MAX_COLS} columns"
            ),
        ),
    };
    if ok {
        return Ok(());
    }
    Err(DrawnError::BadDimensions {
        detail: format!("{what} is {cols}x{rows}, but this kind needs {expected}"),
    })
}

/// Build one runtime frame from a document frame.
///
/// `what` names the frame in the error message ("frame 2", "the reaction
/// frame"), so a broken document says which frame is at fault.
fn build_frame(frame: &FrameDoc, shape: FrameShape, what: &str) -> Result<AsciiSprite, DrawnError> {
    check_frame_shape(shape, frame.cols, frame.rows, what)?;
    let cells = frame
        .cols
        .checked_mul(frame.rows)
        .ok_or_else(|| DrawnError::BadDimensions {
            detail: format!("{what} declares an impossible size"),
        })?;
    if frame.chars.len() != cells || frame.colors.len() != cells {
        return Err(DrawnError::BadDimensions {
            detail: format!(
                "{what} has {} chars and {} colours, but declares {cells} cells",
                frame.chars.len(),
                frame.colors.len()
            ),
        });
    }
    AsciiSprite::from_cells(
        frame.cols,
        frame.rows,
        frame.chars.clone(),
        frame.colors.clone(),
    )
    .ok_or_else(|| DrawnError::BadDimensions {
        detail: format!("{what} does not fit its declared cells"),
    })
}

/// The document frame of a runtime frame.
fn frame_to_doc(sprite: &AsciiSprite) -> FrameDoc {
    FrameDoc {
        cols: sprite.cols(),
        rows: sprite.rows(),
        chars: sprite.chars().to_vec(),
        colors: sprite.colors().to_vec(),
    }
}

/// Check that a runtime asset matches the shape its kind requires, so a
/// broken asset never reaches the disk.
fn validate_runtime(sprite: &DrawnSprite) -> Result<(), DrawnError> {
    let shape = frame_shape(sprite.kind, sprite.size)?;
    if sprite.frames.is_empty() {
        return Err(DrawnError::BadDimensions {
            detail: "the asset has no frames".to_string(),
        });
    }
    for (index, frame) in sprite.frames.iter().enumerate() {
        check_frame_shape(shape, frame.cols(), frame.rows(), &format!("frame {index}"))?;
    }
    if let Some(reaction) = sprite.reaction.as_ref() {
        check_frame_shape(
            shape,
            reaction.cols(),
            reaction.rows(),
            "the reaction frame",
        )?;
    }
    Ok(())
}

/// Stamp one drawn frame onto the grid.
///
/// The cells are walked as one flat row-major run, so there is no loop
/// inside a loop (audit nesting rule). Spaces stay transparent, and
/// off-grid cells are dropped by [`Grid::set_if_visible`] — the
/// non-wrapping write — so a drawn sprite never reappears on the opposite
/// edge of the screen.
fn stamp_drawn_sprite(grid: &mut Grid, sprite: &AsciiSprite, col: i64, row: i64) {
    let cols = sprite.cols();
    if cols == 0 {
        return;
    }
    for (index, (ch, color)) in sprite.chars().iter().zip(sprite.colors()).enumerate() {
        if *ch == ' ' {
            continue;
        }
        let x = col + (index % cols) as i64;
        let y = row + (index / cols) as i64;
        grid.set_if_visible(x, y, *ch, *color);
    }
}

/// Stamp one drawn frame onto the grid, compressed vertically by
/// `squash`.
///
/// The row mapping is the one the built-in art uses
/// ([`crate::sprites::squash_row_shift`]): the frame's bottom row keeps
/// its place and the top comes down, so a duck squashes onto its feet.
fn stamp_drawn_sprite_squashed(
    grid: &mut Grid,
    sprite: &AsciiSprite,
    col: i64,
    row: i64,
    squash: f32,
) {
    let cols = sprite.cols();
    if cols == 0 {
        return;
    }
    let height = sprite.rows();
    for (index, (ch, color)) in sprite.chars().iter().zip(sprite.colors()).enumerate() {
        if *ch == ' ' {
            continue;
        }
        let source_row = index / cols;
        let x = col + (index % cols) as i64;
        let y =
            row + source_row as i64 + crate::sprites::squash_row_shift(height, source_row, squash);
        grid.set_if_visible(x, y, *ch, *color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder that is not the user's real config folder.
    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ducks-drawn-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// A big sprite with one frame of the built-in size and two inked cells.
    fn big_sprite() -> DrawnSprite {
        let (cols, rows) = sprite_frame_size(DrawnSize::Big);
        let mut frame = AsciiSprite::new(cols, rows);
        frame.set(0, 0, 'x', PALETTE[0]);
        frame.set(1, 0, 'o', PALETTE[1]);
        DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: vec![frame],
            reaction: None,
        }
    }

    /// A balloon with one frame of the default authoring disc (109 x 73).
    fn balloon_sprite() -> DrawnSprite {
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        let mut frame = AsciiSprite::new(geometry.cols, geometry.rows);
        frame.set(geometry.cols / 2, geometry.rows / 2, 'o', PALETTE[5]);
        DrawnSprite {
            kind: DrawnKind::Balloon,
            size: None,
            frames: vec![frame],
            reaction: None,
        }
    }

    #[test]
    fn palette_has_eight_colours() {
        assert_eq!(PALETTE.len(), 8);
        assert_eq!(PALETTE_NAMES.len(), 8);
        assert_eq!(PALETTE[0], [255, 255, 255], "white is the default colour");
        assert_eq!(PALETTE_NAMES[0], "WHITE");
        assert!(
            !PALETTE.contains(&[0, 0, 0]),
            "pure black must stay out of the palette (D3)"
        );
        for (index, color) in PALETTE.iter().enumerate() {
            assert!(
                !PALETTE[index + 1..].contains(color),
                "palette colour {index} is duplicated"
            );
        }
    }

    #[test]
    fn tools_contain_the_nineteen_glyphs() {
        assert_eq!(TOOLS.len(), 19);
        let expected = [
            'X', 'x', '.', '/', '\\', 'o', 'O', '`', '_', '-', '~', '|', '+', '*', ',', '<', '>',
            '^', '#',
        ];
        assert_eq!(TOOLS, expected);
    }

    #[test]
    fn default_tool_is_x() {
        assert_eq!(TOOLS[DEFAULT_TOOL], 'x');
    }

    #[test]
    fn default_colour_is_white() {
        assert_eq!(PALETTE[DEFAULT_COLOR], [255, 255, 255]);
        assert_eq!(PALETTE_NAMES[DEFAULT_COLOR], "WHITE");
    }

    #[test]
    fn doc_round_trips() {
        let sprite = big_sprite();
        let doc = sprite.to_doc();
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: DrawnDoc = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, doc, "the document must survive JSON unchanged");
        let runtime = DrawnSprite::from_doc(&back).expect("from doc");
        assert_eq!(
            runtime.to_doc(),
            doc,
            "to_doc -> from_doc -> to_doc is identity"
        );
        assert_eq!(runtime.kind, DrawnKind::Sprite);
        assert_eq!(runtime.size, Some(DrawnSize::Big));
        assert_eq!(runtime.frame_count(), 1);
        assert_eq!(
            runtime.frame(0).map(|f| (f.cols(), f.rows())),
            Some((150, 70))
        );
    }

    #[test]
    fn doc_with_future_version_is_rejected() {
        let mut doc = big_sprite().to_doc();
        doc.version = DOC_VERSION + 1;
        let err = DrawnSprite::from_doc(&doc).expect_err("a future version must be rejected");
        assert!(
            err.to_string().contains(&(DOC_VERSION + 1).to_string()),
            "the error must name the version: {err}"
        );
        assert!(matches!(err, DrawnError::UnsupportedVersion { .. }));
    }

    #[test]
    fn doc_with_bad_length_is_rejected() {
        let mut short_chars = big_sprite().to_doc();
        short_chars.frames[0].chars.pop();
        let err = DrawnSprite::from_doc(&short_chars).expect_err("short chars must be rejected");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");

        let mut long_colors = big_sprite().to_doc();
        long_colors.frames[0].colors.push([0, 0, 0]);
        let err = DrawnSprite::from_doc(&long_colors).expect_err("long colors must be rejected");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn big_sprite_frame_is_150x70() {
        assert_eq!(sprite_frame_size(DrawnSize::Big), (150, 70));
        assert_eq!(
            sprite_frame_size(DrawnSize::Big),
            (crate::sprites::BIG_DUCK_COLS, crate::sprites::BIG_DUCK_ROWS)
        );
        let sprite = big_sprite();
        let frame = &sprite.frames[0];
        assert_eq!((frame.cols(), frame.rows()), (150, 70));
        assert_ne!(
            frame.rows(),
            sprite_canvas_side(DrawnSize::Big),
            "the frame is not the canvas"
        );
    }

    #[test]
    fn small_sprite_frame_is_75x35() {
        assert_eq!(sprite_frame_size(DrawnSize::Small), (75, 35));
        assert_eq!(
            sprite_frame_size(DrawnSize::Small),
            (
                crate::sprites::SMALL_DUCK_COLS,
                crate::sprites::SMALL_DUCK_ROWS
            )
        );
        let (cols, rows) = sprite_frame_size(DrawnSize::Small);
        let frame = AsciiSprite::new(cols, rows);
        let sprite = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Small),
            frames: vec![frame],
            reaction: None,
        };
        assert_eq!(
            sprite.frames[0].rows(),
            35,
            "the small frame is the built-in one"
        );
    }

    #[test]
    fn sprite_canvas_is_square_and_taller_than_the_frame() {
        for (size, side, frame) in [
            (DrawnSize::Big, 150usize, (150usize, 70usize)),
            (DrawnSize::Small, 75usize, (75usize, 35usize)),
        ] {
            assert_eq!(sprite_canvas_side(size), side);
            assert_eq!(sprite_frame_size(size), frame);
            assert!(
                side > frame.1,
                "the canvas must be taller than the frame ({side} vs {})",
                frame.1
            );
        }
        assert_eq!(
            sprite_canvas_side(DrawnSize::Big),
            crate::sprites::BIG_DUCK_COLS
        );
        assert_eq!(
            sprite_canvas_side(DrawnSize::Small),
            crate::sprites::SMALL_DUCK_COLS
        );
    }

    #[test]
    fn sprite_active_rows_match_the_frame_height() {
        assert_eq!(sprite_active_rows(DrawnSize::Big), 70);
        assert_eq!(sprite_active_rows(DrawnSize::Small), 35);
        assert_eq!(
            sprite_active_rows(DrawnSize::Big),
            sprite_frame_size(DrawnSize::Big).1
        );
        assert_eq!(
            sprite_active_rows(DrawnSize::Small),
            sprite_frame_size(DrawnSize::Small).1
        );
    }

    #[test]
    fn balloon_mask_marks_corners_inactive() {
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        let (cols, rows) = (geometry.cols, geometry.rows);
        assert_eq!((cols, rows), (109, 73), "the frame is the disc");
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        let mask = crate::balloon::balloon_authoring_mask(&geometry);
        assert_eq!(mask.len(), cols * rows);
        let at = |x: usize, y: usize| mask[y * cols + x];
        assert!(!at(0, 0), "top-left corner must be inactive");
        assert!(!at(cols - 1, 0), "top-right corner must be inactive");
        assert!(!at(0, rows - 1), "bottom-left corner must be inactive");
        assert!(
            !at(cols - 1, rows - 1),
            "bottom-right corner must be inactive"
        );
        assert!(at(cols / 2, rows / 2), "the centre must be active");
        assert!(at(0, rows / 2), "the left edge mid-row must be active");
        assert!(at(cols / 2, 0), "the top edge mid-column must be active");
    }

    #[test]
    fn slot_mapping_matches_the_parade() {
        assert_eq!(big_slot(1), Some(0));
        assert_eq!(big_slot(2), Some(1));
        assert_eq!(big_slot(0), None);
        assert_eq!(big_slot(3), None);
        assert_eq!(small_slot(1), Some(2));
        assert_eq!(small_slot(9), Some(10));
        assert_eq!(small_slot(0), None);
        assert_eq!(small_slot(10), None);
        assert_eq!(balloon_slot(1), Some(0));
        assert_eq!(balloon_slot(5), Some(4));
        assert_eq!(balloon_slot(0), None);
        assert_eq!(balloon_slot(6), None);

        // The mapping must agree with the parade's own tables.
        assert_eq!(small_slot(9), Some(crate::parade::DUCK_COUNT - 1));
        assert_eq!(big_slot(2), Some(crate::balloon::BIG_DUCK_SLOTS[1]));
        assert_eq!(small_slot(1), Some(crate::parade::BIG_SLOTS));
        assert_eq!(
            big_slot(crate::balloon::BIG_DUCK_SLOTS.len()),
            Some(crate::balloon::BIG_DUCK_SLOTS[1])
        );
        assert_eq!(
            balloon_slot(crate::balloon::BALLOON_HOLDER_DUCK.len()),
            Some(crate::balloon::BALLOON_HOLDER_DUCK.len() - 1)
        );
    }

    #[test]
    fn save_load_round_trip_in_temp_dir() {
        let root = temp_root("round-trip");
        let sprite = big_sprite();
        let path = save_to(&root, &sprite, DrawnKind::Sprite, "sprite-1").expect("save");
        assert!(path.is_file(), "the document must be written");
        assert!(path.ends_with("sprite-1.json"));
        let loaded = load_from(&root, DrawnKind::Sprite, "sprite-1").expect("load");
        assert_eq!(loaded.to_doc(), sprite.to_doc(), "the asset must survive");
        assert_eq!(loaded.kind, DrawnKind::Sprite);
        assert_eq!(loaded.size, Some(DrawnSize::Big));
        assert_eq!(loaded.frame_count(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_is_sorted() {
        let root = temp_root("list");
        let sprite = big_sprite();
        for name in ["sprite-2", "sprite-1", "sprite-10"] {
            save_to(&root, &sprite, DrawnKind::Sprite, name).expect("save");
        }
        std::fs::write(root.join("notes.txt"), b"x").expect("write stray file");
        let names = list_in(&root);
        assert_eq!(
            names,
            vec![
                "sprite-1".to_string(),
                "sprite-10".to_string(),
                "sprite-2".to_string()
            ],
            "the names must be sorted and only json files counted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn next_free_name_skips_used() {
        let root = temp_root("next-name");
        let sprite = big_sprite();
        assert_eq!(next_free_name_in(&root, DrawnKind::Sprite), "sprite-1");
        save_to(&root, &sprite, DrawnKind::Sprite, "sprite-1").expect("save");
        assert_eq!(
            next_free_name_in(&root, DrawnKind::Sprite),
            "sprite-2",
            "a used name must be skipped"
        );
        assert_eq!(next_free_name_in(&root, DrawnKind::Balloon), "balloon-1");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A re-save keeps a best-effort `.bak` of the previous version — an
    /// ordinary re-save used to destroy it with no way back (audit
    /// 2026-10-23 §F-061). The `.bak` is not offered as an asset.
    #[test]
    fn a_resave_keeps_a_backup_of_the_previous_version() {
        let root = temp_root("resave-backup");
        let first = big_sprite();
        save_to(&root, &first, DrawnKind::Sprite, "sprite-1").expect("the first save");
        let mut second = big_sprite();
        second.frames[0].set(5, 5, 'o', PALETTE[2]);
        save_to(&root, &second, DrawnKind::Sprite, "sprite-1").expect("the re-save");

        let backup = root.join("sprite-1.json.bak");
        assert!(backup.is_file(), "the previous version is kept");
        let backed_up =
            load_from(&root, DrawnKind::Sprite, "sprite-1").expect("the current version loads");
        assert_eq!(
            backed_up.to_doc(),
            second.to_doc(),
            "the file holds the new version"
        );
        assert_eq!(
            list_in(&root),
            vec!["sprite-1".to_string()],
            "the backup is not offered as an asset"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The size listing skips a file over the document cap instead of
    /// parsing it whole (audit 2026-10-23 §F-054).
    #[test]
    fn the_size_listing_skips_oversized_files() {
        let root = temp_root("oversized");
        save_to(&root, &big_sprite(), DrawnKind::Sprite, "small").expect("save");
        let oversized = vec![b'x'; MAX_DOC_BYTES as usize + 1];
        std::fs::write(root.join("huge.json"), &oversized).expect("write the oversized file");

        let entries = list_with_sizes_in(&root);
        assert_eq!(
            entries.len(),
            1,
            "only the capped-size asset is listed: {entries:?}"
        );
        assert_eq!(entries[0].name, "small");
        assert_eq!(entries[0].size, Some(DrawnSize::Big));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn illegal_names_are_refused() {
        let root = temp_root("illegal");
        let sprite = big_sprite();
        let too_long = "x".repeat(MAX_NAME_LEN + 1);
        let names = [
            "",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "Cat",
            "with space",
            "dot.name",
            too_long.as_str(),
        ];
        for name in names {
            let err = save_to(&root, &sprite, DrawnKind::Sprite, name)
                .expect_err("an illegal name must be refused");
            assert!(
                matches!(err, DrawnError::InvalidName { .. }),
                "{name}: {err:?}"
            );
            let err = load_from(&root, DrawnKind::Sprite, name)
                .expect_err("an illegal name must be refused");
            assert!(
                matches!(err, DrawnError::InvalidName { .. }),
                "{name}: {err:?}"
            );
        }
        assert!(
            list_in(&root).is_empty(),
            "nothing may be written for an illegal name"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn draw_clips_instead_of_wrapping() {
        let mut grid = Grid::new(20, 10, 1, 1);
        let frame =
            AsciiSprite::from_cells(8, 1, vec!['x'; 8], vec![[255, 255, 255]; 8]).expect("frame");
        let sprite = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: vec![frame],
            reaction: None,
        };
        sprite.draw(&mut grid, -5, 0, 0);
        assert_eq!(grid.ink_count(), 3, "only the visible part is drawn");
        assert_eq!(grid.char_at(0, 0), Some('x'));
        assert_eq!(
            grid.char_at(19, 0),
            Some(' '),
            "the last column must stay clean — no wrap"
        );
        // Fully off-grid: nothing at all.
        let mut far = Grid::new(20, 10, 1, 1);
        sprite.draw(&mut far, -80, 0, 0);
        assert_eq!(far.ink_count(), 0);
    }

    #[test]
    fn draw_uses_the_per_cell_colour() {
        let mut grid = Grid::new(4, 4, 1, 1);
        let frame = AsciiSprite::from_cells(2, 1, vec!['x', 'o'], vec![PALETTE[1], PALETTE[4]])
            .expect("frame");
        let sprite = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: vec![frame],
            reaction: None,
        };
        sprite.draw(&mut grid, 1, 1, 0);
        assert_eq!(
            grid.color_at(1, 1),
            Some(PALETTE[1]),
            "the cell colour is kept"
        );
        assert_eq!(
            grid.color_at(2, 1),
            Some(PALETTE[4]),
            "the cell colour is kept"
        );
    }

    #[test]
    fn crop_to_active_drops_the_inactive_rows() {
        let side = sprite_canvas_side(DrawnSize::Big);
        let canvas = AsciiSprite::new(side, side);
        let cropped = crop_to_active(&canvas, DrawnKind::Sprite, DrawnSize::Big);
        assert_eq!((cropped.cols(), cropped.rows()), (150, 70));

        let side = sprite_canvas_side(DrawnSize::Small);
        let canvas = AsciiSprite::new(side, side);
        let cropped = crop_to_active(&canvas, DrawnKind::Sprite, DrawnSize::Small);
        assert_eq!((cropped.cols(), cropped.rows()), (75, 35));

        let canvas = AsciiSprite::new(73, 73);
        let cropped = crop_to_active(&canvas, DrawnKind::Balloon, DrawnSize::Big);
        assert_eq!(
            (cropped.cols(), cropped.rows()),
            (73, 73),
            "a balloon canvas is the frame already"
        );
    }

    #[test]
    fn crop_to_active_keeps_the_inked_cells() {
        let side = sprite_canvas_side(DrawnSize::Big);
        let mut canvas = AsciiSprite::new(side, side);
        canvas.set(3, 2, 'x', PALETTE[2]);
        canvas.set(4, 69, 'o', PALETTE[3]);
        let cropped = crop_to_active(&canvas, DrawnKind::Sprite, DrawnSize::Big);
        assert_eq!(cropped.get(3, 2), Some(('x', PALETTE[2])));
        assert_eq!(cropped.get(4, 69), Some(('o', PALETTE[3])));
    }

    #[test]
    fn balloon_document_is_the_disc_frame_with_blank_corners() {
        let sprite = balloon_sprite();
        let doc = sprite.to_doc();
        assert_eq!(doc.kind, DrawnKind::Balloon);
        assert_eq!(doc.size, None);
        assert_eq!(doc.frames.len(), 1);
        assert_eq!((doc.frames[0].cols, doc.frames[0].rows), (109, 73));
        let (cols, rows) = (109, 73);
        let corner = |x: usize, y: usize| doc.frames[0].chars[y * cols + x];
        assert_eq!(corner(0, 0), ' ', "the corner cells stay blank");
        assert_eq!(corner(cols - 1, rows - 1), ' ');
        let runtime = DrawnSprite::from_doc(&doc).expect("from doc");
        assert_eq!(runtime.frame_count(), 1);
    }

    #[test]
    fn a_balloon_frame_must_be_an_authoring_disc() {
        let mut doc = balloon_sprite().to_doc();
        // The old square frame: the shape that stretched the art 1.5x.
        doc.frames[0].cols = 73;
        doc.frames[0].rows = 73;
        let err = DrawnSprite::from_doc(&doc).expect_err("a square balloon is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
        assert!(err.to_string().contains("authoring disc"), "{err}");
    }

    #[test]
    fn a_balloon_frame_must_hold_its_whole_disc() {
        let mut doc = balloon_sprite().to_doc();
        doc.frames[0].rows = 72;
        let err = DrawnSprite::from_doc(&doc).expect_err("an even balloon frame is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn a_sprite_document_needs_a_size() {
        let mut doc = big_sprite().to_doc();
        doc.size = None;
        let err = DrawnSprite::from_doc(&doc).expect_err("a sizeless sprite is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn a_sprite_frame_must_have_the_builtin_size() {
        let mut doc = big_sprite().to_doc();
        doc.frames[0].rows = 71;
        let err = DrawnSprite::from_doc(&doc).expect_err("a wrong frame size is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn loading_a_missing_asset_reports_not_found() {
        let root = temp_root("missing");
        let err = load_from(&root, DrawnKind::Sprite, "sprite-1").expect_err("no such asset");
        assert!(matches!(err, DrawnError::NotFound { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_into_the_wrong_kind_folder_is_refused() {
        let root = temp_root("kind");
        let sprite = big_sprite();
        let err = save_to(&root, &sprite, DrawnKind::Balloon, "sprite-1")
            .expect_err("the kind must match the folder");
        assert!(matches!(err, DrawnError::KindMismatch { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_document_is_reported_as_json_error() {
        let root = temp_root("corrupt");
        std::fs::write(root.join("sprite-1.json"), b"{ not json").expect("write");
        let err = load_from(&root, DrawnKind::Sprite, "sprite-1").expect_err("corrupt json");
        assert!(matches!(err, DrawnError::Json { .. }), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A decoration of the big frame with one inked cell.
    fn big_decoration() -> DrawnSprite {
        let (cols, rows) = sprite_frame_size(DrawnSize::Big);
        let mut frame = AsciiSprite::new(cols, rows);
        frame.set(0, 0, 'x', PALETTE[3]);
        DrawnSprite {
            kind: DrawnKind::Decoration,
            size: Some(DrawnSize::Big),
            frames: vec![frame],
            reaction: None,
        }
    }

    #[test]
    fn a_decoration_document_carries_the_ducks_size() {
        let sprite = big_decoration();
        let doc = sprite.to_doc();
        assert_eq!(doc.kind, DrawnKind::Decoration);
        assert_eq!(doc.size, Some(DrawnSize::Big));
        assert_eq!(
            (doc.frames[0].cols, doc.frames[0].rows),
            (150, 70),
            "a decoration's frame is the duck's frame"
        );
        let runtime = DrawnSprite::from_doc(&doc).expect("from doc");
        assert_eq!(runtime.kind, DrawnKind::Decoration);
        assert_eq!(runtime.size, Some(DrawnSize::Big));
    }

    #[test]
    fn a_decoration_document_needs_a_size() {
        let mut doc = big_decoration().to_doc();
        doc.size = None;
        let err = DrawnSprite::from_doc(&doc).expect_err("a sizeless decoration is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn a_decoration_frame_must_have_the_builtin_size() {
        let mut doc = big_decoration().to_doc();
        doc.frames[0].rows = 71;
        let err = DrawnSprite::from_doc(&doc).expect_err("a wrong frame size is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn the_decoration_prefix_and_folder_are_its_own() {
        assert_eq!(DrawnKind::Decoration.prefix(), "decoration");
        assert_eq!(DrawnKind::Decoration.to_string(), "decoration");
        // A decoration is not a sprite: the two must not share a folder.
        assert_ne!(DrawnKind::Sprite.prefix(), DrawnKind::Decoration.prefix());
    }

    #[test]
    fn a_document_with_no_frames_is_refused() {
        let mut doc = big_sprite().to_doc();
        doc.frames.clear();
        let err = DrawnSprite::from_doc(&doc).expect_err("an empty document is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn frame_count_is_never_zero() {
        let empty = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: Vec::new(),
            reaction: None,
        };
        assert_eq!(empty.frame_count(), 1);
        assert!(empty.frame(0).is_none());
    }

    /// A drawn sprite with a reaction frame: the same frame, marked.
    fn sprite_with_reaction() -> DrawnSprite {
        let mut sprite = big_sprite();
        let (cols, rows) = sprite_frame_size(DrawnSize::Big);
        let mut reaction = AsciiSprite::new(cols, rows);
        reaction.set(0, 0, 'r', PALETTE[2]);
        sprite.reaction = Some(reaction);
        sprite
    }

    #[test]
    fn a_drawn_reaction_frame_round_trips() {
        let sprite = sprite_with_reaction();
        let doc = sprite.to_doc();
        assert!(doc.reaction.is_some(), "the document must carry the frame");
        let json = serde_json::to_string(&doc).expect("serialize");
        let back: DrawnDoc = serde_json::from_str(&json).expect("deserialize");
        let runtime = DrawnSprite::from_doc(&back).expect("from doc");
        assert!(
            runtime.reaction_frame().is_some(),
            "the reaction frame must survive the round trip"
        );
        assert_eq!(runtime.to_doc(), doc, "the document must be unchanged");
    }

    #[test]
    fn an_old_document_without_a_reaction_frame_still_loads() {
        // The version is NOT bumped: the version check is a strict
        // equality, so a bump would refuse every document the user has.
        let mut doc = big_sprite().to_doc();
        doc.reaction = None;
        let json = serde_json::to_string(&doc).expect("serialize");
        assert!(
            !json.contains("reaction"),
            "the field must not be written when it is absent: {json}"
        );
        let back: DrawnDoc = serde_json::from_str(&json).expect("deserialize");
        let runtime = DrawnSprite::from_doc(&back).expect("an old document must load");
        assert!(runtime.reaction_frame().is_none());
        assert_eq!(runtime.frame_count(), 1);
    }

    #[test]
    fn a_bad_reaction_frame_is_refused() {
        let mut doc = sprite_with_reaction().to_doc();
        let reaction = doc.reaction.as_mut().expect("the reaction frame");
        reaction.rows = 71;
        let err = DrawnSprite::from_doc(&doc).expect_err("a wrong frame size is refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");

        let mut doc = sprite_with_reaction().to_doc();
        let reaction = doc.reaction.as_mut().expect("the reaction frame");
        reaction.chars.pop();
        let err = DrawnSprite::from_doc(&doc).expect_err("short chars are refused");
        assert!(matches!(err, DrawnError::BadDimensions { .. }), "{err:?}");
    }

    #[test]
    fn the_squash_keeps_the_feet_on_the_ground() {
        // A squash of zero must draw exactly what the plain draw draws.
        let (cols, rows) = sprite_frame_size(DrawnSize::Big);
        let mut frame = AsciiSprite::new(cols, rows);
        frame.set(0, 0, 'x', PALETTE[0]);
        frame.set(0, rows - 1, 'x', PALETTE[0]);
        let sprite = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: vec![frame],
            reaction: None,
        };
        let mut plain = Grid::new(20, 80, 1, 1);
        sprite.draw(&mut plain, 2, 1, 0);
        let mut squashed = Grid::new(20, 80, 1, 1);
        sprite.draw_squashed(&mut squashed, 2, 1, 0, 0.0);
        assert_eq!(plain, squashed, "no squash must be the plain draw");

        // The feet stay: the bottom row does not move.
        let mut full = Grid::new(20, 80, 1, 1);
        sprite.draw_squashed(&mut full, 2, 1, 0, 1.0);
        assert_eq!(full.char_at(2, rows), Some('x'), "the feet stay planted");
        assert_eq!(plain.char_at(2, rows), Some('x'));
        // The top comes down.
        assert_eq!(plain.char_at(2, 1), Some('x'));
        assert_eq!(full.char_at(2, 1), Some(' '), "the top row must come down");
    }

    #[test]
    fn the_reaction_frame_is_drawn_instead_of_the_walking_frame() {
        let sprite = sprite_with_reaction();
        let mut grid = Grid::new(20, 80, 1, 1);
        assert!(
            sprite.draw_reaction(&mut grid, 2, 1),
            "the reaction frame must be drawn"
        );
        assert_eq!(
            grid.char_at(2, 1),
            Some('r'),
            "the reaction frame is the face"
        );
        let mut bare = DrawnSprite {
            kind: sprite.kind,
            size: sprite.size,
            frames: sprite.frames.clone(),
            reaction: None,
        };
        bare.reaction = None;
        let mut grid = Grid::new(20, 80, 1, 1);
        assert!(
            !bare.draw_reaction(&mut grid, 2, 1),
            "an asset without a face draws nothing"
        );
    }
}

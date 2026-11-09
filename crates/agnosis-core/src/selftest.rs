//! Headless selftest: render scene frames to PNG without a window.
//!
//! `agnosis selftest` exercises the exact CPU raster path the popup shows
//! (scene simulation + [`crate::font::Font`] +
//! [`crate::blit::blit_grid_rows_between`]) and writes one PNG per frame into
//! an output directory. No winit window and no wgpu device are involved, so
//! this runs on GPU-less CI boxes.
//!
//! Deterministic: the same config renders byte-identical PNGs.
//!
//! Output location: the gate writes **only** into the directory it is
//! handed (`--out`). Nothing here touches the repository — the unit tests
//! below point the same code at temp dirs (audit 2026-10-17 § Maintainability:
//! "audit_proto tests write into the repo" was fixed by redirecting that
//! module to a temp dir; this module never had the problem).

use crate::assets::AssetDirs;
use crate::blit::{save_png, FrameBuffer};
use crate::config::Config;
use crate::font::Font;
use crate::scene::Scene;

/// Simulation step per rendered frame: the popup ticks at 30 fps, so the
/// gate frames advance the scene at the same rate.
const GATE_FRAME_SECONDS: f32 = 1.0 / 30.0;

/// Monitor geometry of the default 1080p gate.
const GATE_MONITOR_W: u32 = 1920;
const GATE_MONITOR_H: u32 = 1080;

/// The grid the gate renders at: the cell count and the cell size.
///
/// The geometry is one value, not four arguments: the gate hands it to
/// the scene, to the font and to the framebuffer, and a struct keeps
/// them from drifting apart.
#[derive(Debug, Clone, Copy)]
struct Grid {
    /// Cells across the screen.
    cols: usize,
    /// Cells down the screen.
    rows: usize,
    /// Cell width in pixels.
    cell_w: u32,
    /// Cell height in pixels.
    cell_h: u32,
}

impl Grid {
    /// The grid the config asks for on a `monitor_w x monitor_h` panel.
    fn for_monitor(cfg: &Config, monitor_w: u32, monitor_h: u32) -> Self {
        let (cols, rows, cell_w, cell_h) = cfg.grid_for_monitor(monitor_w, monitor_h, 1.0);
        Self {
            cols,
            rows,
            cell_w,
            cell_h,
        }
    }

    /// True when the grid would draw nothing at all.
    fn is_degenerate(&self) -> bool {
        self.cols == 0 || self.rows == 0 || self.cell_w == 0 || self.cell_h == 0
    }
}

/// Selftest failures.
#[derive(Debug, thiserror::Error)]
pub enum SelftestError {
    /// Config values could not produce a usable grid.
    #[error("config error: {0}")]
    Config(String),
    /// The embedded font failed to load.
    #[error("font error: {0}")]
    Font(String),
    /// A PNG could not be encoded or written.
    #[error("image error: {0}")]
    Image(String),
    /// Filesystem error creating the output dir or writing a frame.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Render `frames` scene frames (30fps sim) headlessly through Font+blit
/// and write PNGs to `out_dir` (created if missing).
///
/// Geometry comes from the config's `grid_for_monitor` at 1920x1080: the
/// deterministic 1080p gate the byte-identical test pins. Use
/// [`run_for_monitor`] for other panels (the 4K case below).
///
/// `assets` is where the drawn art and the MIDI tracks live: the gate
/// renders the **user's** parade, so the shell hands the folders over.
/// The core never looks them up itself.
///
/// Returns the number of frames written.
pub fn run(
    cfg: &Config,
    frames: u32,
    out_dir: &std::path::Path,
    assets: &AssetDirs,
) -> Result<u32, SelftestError> {
    run_for_monitor(cfg, frames, out_dir, GATE_MONITOR_W, GATE_MONITOR_H, assets)
}

/// Same as [`run`] at an explicit monitor size: the 4K gate renders the same
/// deterministic scene at 3840x2160 so the panel the audit targets stays
/// pinned too.
pub fn run_for_monitor(
    cfg: &Config,
    frames: u32,
    out_dir: &std::path::Path,
    monitor_w: u32,
    monitor_h: u32,
    assets: &AssetDirs,
) -> Result<u32, SelftestError> {
    if frames == 0 {
        return Ok(0);
    }
    if monitor_w == 0 || monitor_h == 0 {
        return Err(SelftestError::Config("degenerate selftest monitor".into()));
    }
    std::fs::create_dir_all(out_dir)?;
    let grid = Grid::for_monitor(cfg, monitor_w, monitor_h);
    render_frames(cfg, frames, out_dir, grid, assets)
}

/// Guard the grid/pixel geometry, then render every frame to PNG.
fn render_frames(
    cfg: &Config,
    frames: u32,
    out_dir: &std::path::Path,
    grid: Grid,
    assets: &AssetDirs,
) -> Result<u32, SelftestError> {
    if grid.is_degenerate() {
        return Err(SelftestError::Config("degenerate selftest geometry".into()));
    }
    let mut font =
        Font::new(grid.cell_w, grid.cell_h).map_err(|e| SelftestError::Font(e.to_string()))?;
    let mut scene =
        Scene::new_with_assets(cfg, grid.cols, grid.rows, grid.cell_w, grid.cell_h, assets);
    // The headless paths are not the UI thread, so the backdrops are
    // converted here: the gate must show the picture the app shows. The
    // pictures are **absolute** paths: the shell resolved the configured
    // names before the core saw the config.
    let tiles = crate::backdrop::convert_layers(&cfg.backdrop);
    scene.set_backdrops(tiles);
    let width = grid.cols as u32 * grid.cell_w;
    let height = grid.rows as u32 * grid.cell_h;
    if width == 0 || height == 0 {
        return Err(SelftestError::Config("degenerate selftest pixels".into()));
    }
    let mut fb = FrameBuffer::new(width, height);
    for i in 0..frames {
        scene.advance(GATE_FRAME_SECONDS);
        blit_scene_into(&scene, &mut font, &mut fb);
        let path = out_dir.join(format!("frame_{i:04}.png"));
        save_png(&fb, &path).map_err(|e| SelftestError::Image(e.to_string()))?;
    }
    Ok(frames)
}

/// Clear the framebuffer and blit the scene with the popup's two-pass offset
/// split: parade rows scroll with the sub-cell offset, grass/HUD/input rows
/// blit at offset 0.
///
/// The composition itself is [`crate::blit::blit_scene`], the **same**
/// function the GPU popup uses: the gate and the app cannot drift, and an
/// exported frame is the app's frame.
pub fn blit_scene_into(scene: &Scene, font: &mut Font, fb: &mut FrameBuffer) {
    let layers = scene.backdrop_blits();
    let overlay = scene.message_blit();
    crate::blit::blit_scene(
        fb,
        font,
        scene.grid(),
        scene.sub_cell_offset_px(),
        scene.ground_row() as usize,
        scene.sky_ramp(),
        &layers,
        overlay.as_ref(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blit::blit_grid_rows;

    /// Grid geometry of the 1080p gate: 1920x1080 at the row-budget cell
    /// `[2,3]` resolves to a 960x360 grid.
    const GATE_GRID_COLS: usize = 960;
    const GATE_GRID_ROWS: usize = 360;
    const GATE_CELL_W: u32 = 2;
    const GATE_CELL_H: u32 = 3;

    /// Monitor geometry of the 4K gate (the panel the audit targets).
    const MONITOR_4K_W: u32 = 3840;
    const MONITOR_4K_H: u32 = 2160;

    fn unique_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ducks-selftest-{}-{nanos}-{tag}",
            std::process::id()
        ))
    }

    fn png_dims(path: &std::path::Path) -> (u32, u32) {
        let img = image::open(path).expect("reload selftest png").to_rgba8();
        (img.width(), img.height())
    }

    #[test]
    fn renders_three_frames_with_expected_dimensions() {
        let dir = unique_dir("dims");
        let n = run(&Config::default(), 3, &dir, &AssetDirs::default()).expect("selftest run");
        assert_eq!(n, 3);
        for i in 0..3 {
            assert_frame_dims(&dir, i, GATE_MONITOR_W, GATE_MONITOR_H);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One rendered frame exists at `dir/frame_{i:04}.png` with the expected
    /// pixel dimensions.
    fn assert_frame_dims(dir: &std::path::Path, i: u32, w: u32, h: u32) {
        let path = dir.join(format!("frame_{i:04}.png"));
        assert!(path.is_file(), "missing {}", path.display());
        assert_eq!(png_dims(&path), (w, h));
    }

    #[test]
    fn two_runs_are_byte_identical() {
        // Two independent renders: each `run` call builds its own Scene,
        // Font and FrameBuffer and writes its own directory, so the bytes
        // compared below come from two separate render passes — never from
        // one render compared with itself.
        let a = unique_dir("det-a");
        let b = unique_dir("det-b");
        assert_ne!(a, b, "the two runs must use separate output dirs");
        assert_eq!(
            run(&Config::default(), 3, &a, &AssetDirs::default()).unwrap(),
            3
        );
        assert_eq!(
            run(&Config::default(), 3, &b, &AssetDirs::default()).unwrap(),
            3
        );
        assert_frame_dims(&a, 0, GATE_MONITOR_W, GATE_MONITOR_H);
        assert_frame_dims(&b, 0, GATE_MONITOR_W, GATE_MONITOR_H);
        assert_dirs_byte_identical(&a, &b, 3);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    /// Every `frame_{i:04}.png` for `i in 0..frames` is byte-identical
    /// between two selftest output dirs (the `two_runs_are_byte_identical`
    /// semantics, shared with the 4K case).
    fn assert_dirs_byte_identical(a: &std::path::Path, b: &std::path::Path, frames: u32) {
        for i in 0..frames {
            let pa = std::fs::read(a.join(format!("frame_{i:04}.png"))).unwrap();
            let pb = std::fs::read(b.join(format!("frame_{i:04}.png"))).unwrap();
            assert_eq!(pa, pb, "frame {i} differs between runs");
        }
    }

    #[test]
    fn two_runs_are_byte_identical_at_4k() {
        // Second deterministic gate alongside the fixed 1080p one: the 4K
        // panel the audit targets (3840x2160 -> 960x360 grid at the [4,6]
        // cell) renders byte-identical PNGs run to run.
        //
        // Two independent renders: the two `run_for_monitor` calls each
        // build their own Scene, Font and FrameBuffer and write their own
        // directory, so the comparison below is between two separate render
        // passes, not one render against itself. Both frames of both runs
        // are checked for the real 4K pixel size, so the gate cannot pass
        // on a silently-downscaled render.
        let a = unique_dir("det4k-a");
        let b = unique_dir("det4k-b");
        assert_ne!(a, b, "the two runs must use separate output dirs");
        assert_eq!(
            run_for_monitor(
                &Config::default(),
                2,
                &a,
                MONITOR_4K_W,
                MONITOR_4K_H,
                &AssetDirs::default(),
            )
            .unwrap(),
            2
        );
        assert_eq!(
            run_for_monitor(
                &Config::default(),
                2,
                &b,
                MONITOR_4K_W,
                MONITOR_4K_H,
                &AssetDirs::default(),
            )
            .unwrap(),
            2
        );
        for i in 0..2 {
            assert_frame_dims(&a, i, MONITOR_4K_W, MONITOR_4K_H);
            assert_frame_dims(&b, i, MONITOR_4K_W, MONITOR_4K_H);
        }
        assert_dirs_byte_identical(&a, &b, 2);
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn visual_proof_beaks_order_hud_grass() {
        let dir = unique_dir("visual-proof");
        assert_eq!(
            run(&Config::default(), 1, &dir, &AssetDirs::default()).unwrap(),
            1
        );
        let path = dir.join("frame_0000.png");
        let img = image::open(&path).expect("reload proof png").to_rgba8();
        assert_eq!(
            (img.width(), img.height()),
            (GATE_MONITOR_W, GATE_MONITOR_H)
        );
        let (cols, rows, cell_w, cell_h) =
            Config::default().grid_for_monitor(GATE_MONITOR_W, GATE_MONITOR_H, 1.0);
        assert_eq!(
            (cols, rows, cell_w, cell_h),
            (GATE_GRID_COLS, GATE_GRID_ROWS, GATE_CELL_W, GATE_CELL_H)
        );
        let mut scene = Scene::new_with_assets(
            &Config::default(),
            cols,
            rows,
            cell_w,
            cell_h,
            &AssetDirs::default(),
        );
        scene.advance(GATE_FRAME_SECONDS);
        let text = scene.grid().to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), rows);
        // X-art ducks: the band above the grass must carry duck ink, and the
        // west-most X must sit inside the lead slot (slots are evenly
        // spaced; slot 0, a big duck, is the leftmost on screen).
        let ground = crate::scene::ground_row_for(rows) as usize;
        assert_eq!(ground, 288, "grass must sit at 4/5 height");
        let (duck_ink, leftmost_x) = duck_ink_above_grass(&lines, ground);
        println!("VISUALPROOF duck X ink above grass: {duck_ink}");
        assert!(
            duck_ink >= 1000,
            "X-art ducks must ink the band above the grass, got {duck_ink}"
        );
        let lead_width = cols / crate::parade::DUCK_COUNT;
        assert!(
            leftmost_x.unwrap() < lead_width,
            "lead duck must be leftmost, got {leftmost_x:?} (lead slot width {lead_width})"
        );
        let wave_line = lines[ground + 1];
        let grass_ink = wave_line.chars().filter(|&c| c != ' ').count();
        assert!(
            grass_ink >= 100,
            "grass wave band must span row {}, got {grass_ink}",
            ground + 1
        );
        // Block-font HUD: X ink in the 5 rows above the bottom margin, in
        // the configured bottom-right corner.
        let hud_ink: usize = lines[rows - 5..]
            .iter()
            .flat_map(|l| l.chars().skip(cols * 2 / 3).filter(|&c| c == 'X'))
            .count();
        assert!(
            hud_ink >= 40,
            "block-font HUD must render bottom-right, got {hud_ink}"
        );
        let u0 = scene_grid_unwrapped(&mut Config::default(), 0);
        let (u2, u10) = (
            scene_grid_unwrapped(&mut Config::default(), 2),
            scene_grid_unwrapped(&mut Config::default(), 10),
        );
        assert!(
            u0 < u2 && u2 < u10,
            "slot order 0<2<10, got {u0},{u2},{u10}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `(duck_ink, leftmost_x)` in the band above `ground`: total `X` cells
    /// and the west-most `X` column (audit nesting: the line/char loops live
    /// in [`count_line_ink`] so the test body stays flat).
    fn duck_ink_above_grass(lines: &[&str], ground: usize) -> (usize, Option<usize>) {
        lines
            .iter()
            .take(ground)
            .map(|line| count_line_ink(line))
            .fold((0, None), |(ink, west), (n, m)| (ink + n, min_opt(west, m)))
    }

    /// Smaller of two optional columns: `None` means "no ink yet", so a
    /// one-sided `Some` wins (`Option::min` would keep `None`).
    fn min_opt(a: Option<usize>, b: Option<usize>) -> Option<usize> {
        match (a, b) {
            (None, b) => b,
            (a, None) => a,
            (Some(x), Some(y)) => Some(x.min(y)),
        }
    }

    /// `(ink, west)` for one grid line: `X` count and west-most `X` column.
    fn count_line_ink(line: &str) -> (usize, Option<usize>) {
        line.chars()
            .enumerate()
            .filter(|(_, ch)| *ch == 'X')
            .fold((0, None), |(ink, west), (c, _)| {
                (ink + 1, Some(west.map_or(c, |m: usize| m.min(c))))
            })
    }

    #[test]
    fn png_bytes_show_green_grass_band_and_ordered_ducks() {
        let dir = unique_dir("png-proof");
        assert_eq!(
            run(&Config::default(), 1, &dir, &AssetDirs::default()).unwrap(),
            1
        );
        let path = dir.join("frame_0000.png");
        let img = image::open(&path).expect("reload proof png").to_rgba8();
        let (w, h) = (img.width(), img.height());
        assert_eq!((w, h), (GATE_MONITOR_W, GATE_MONITOR_H));
        let (cols, cell_w, cell_h) = (GATE_GRID_COLS, GATE_CELL_W, GATE_CELL_H);
        let row_score = |r: u32| -> (u32, u32) {
            (0..cols as u32).fold((0, 0), |(green, yellow), c| {
                let (g, y) = cell_score(&img, c, r, cell_w, cell_h);
                (green + g, yellow + y)
            })
        };
        let ground = crate::scene::ground_row_for(GATE_GRID_ROWS) as u32;
        let (g_wave, y_wave) = row_score(ground + 1);
        let (g_duck, y_duck) = row_score(280);
        println!(
            "PNGPROOF wave row {} green {g_wave} yellow {y_wave}; duck280 green {g_duck} yellow {y_duck}",
            ground + 1
        );
        assert!(
            g_wave >= 800,
            "grass PNG wave band must sit at row {}, got {g_wave}",
            ground + 1
        );
        assert!(
            g_duck <= 100,
            "row 280 must not read as grass, got {g_duck}"
        );
        assert!(
            y_duck >= 10,
            "duck yellow must show above the grass, got {y_duck}"
        );
        // Order check: the west-most X above the grass belongs to the lead
        // (big) duck — slots are evenly spaced and slot 0 is leftmost.
        let mut scene = Scene::new_with_assets(
            &Config::default(),
            GATE_GRID_COLS,
            GATE_GRID_ROWS,
            GATE_CELL_W,
            GATE_CELL_H,
            &AssetDirs::default(),
        );
        scene.advance(GATE_FRAME_SECONDS);
        let leftmost_x = scene
            .grid()
            .to_text()
            .lines()
            .take(ground as usize)
            .filter_map(|l| l.find('X'))
            .min();
        assert!(leftmost_x.is_some(), "X-art ducks must render");
        let lead_width = GATE_GRID_COLS / crate::parade::DUCK_COUNT;
        assert!(
            leftmost_x.unwrap() < lead_width,
            "big ducks must head the train left of the smalls, leftmost X {leftmost_x:?} (lead slot width {lead_width})"
        );
        println!("PNGPROOF leftmost duck X col {leftmost_x:?} (lead slot width {lead_width})");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn scene_grid_unwrapped(_cfg: &mut Config, _slot: usize) -> f32 {
        let p = crate::parade::Parade::new(160, 20.0);
        p.unwrapped_x(_slot)
    }

    /// `(green_cells, yellow_cells)` for one pixel in the grid row `r`:
    /// each cell counts once when any of its pixels reads grass-green /
    /// duck-yellow (audit nesting site #6: the dy/dx pixel loops live here
    /// so the `row_score` closure stays flat).
    fn cell_score(
        img: &image::RgbaImage,
        col: u32,
        row: u32,
        cell_w: u32,
        cell_h: u32,
    ) -> (u32, u32) {
        let (green, yellow) = (0..cell_h).fold((0u32, 0u32), |(green, yellow), dy| {
            let (g, y) = score_cell_row(img, col, row, dy, cell_w, cell_h);
            (green + g, yellow + y)
        });
        (green.min(1), yellow.min(1))
    }

    /// `(green_px, yellow_px)` in one pixel row of the cell: counts every
    /// grass-green / duck-yellow pixel (the dx loop; the dy loop lives in
    /// [`cell_score`], so neither function nests past depth 1).
    fn score_cell_row(
        img: &image::RgbaImage,
        col: u32,
        row: u32,
        dy: u32,
        cell_w: u32,
        cell_h: u32,
    ) -> (u32, u32) {
        (0..cell_w).fold((0u32, 0u32), |(green, yellow), dx| {
            let p = img.get_pixel(col * cell_w + dx, row * cell_h + dy);
            (
                green + is_grass_pixel(p) as u32,
                yellow + is_duck_pixel(p) as u32,
            )
        })
    }

    /// Grass-green: green channel dominant and bright enough.
    fn is_grass_pixel(p: &image::Rgba<u8>) -> bool {
        p[1] >= 60 && p[1] >= p[0] && p[1] >= p[2]
    }

    /// Duck-yellow: warm bright (high red+green, low blue).
    fn is_duck_pixel(p: &image::Rgba<u8>) -> bool {
        p[0] > 150 && p[1] > 120 && p[2] < 140
    }

    #[test]
    fn zero_frames_writes_nothing_and_returns_zero() {
        let dir = unique_dir("zero");
        assert_eq!(
            run(&Config::default(), 0, &dir, &AssetDirs::default()).unwrap(),
            0
        );
        assert!(!dir.exists() || dir.read_dir().unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fix2_sprite_proof_png_ducks_at_audit_scale() {
        // Fix 2 gate: the 150x70 / 75x35 X-art ducks rendered through the
        // real CPU raster path (Grid -> Font -> blit -> PNG) at the audit
        // cell [2,3] on a 960x360 grid. Ducks are placed directly — the
        // parade/train redesign is Fix 4. The proof PNG goes to a temp dir
        // (tests must not write into the repo); this gates shape.
        use crate::grid::Grid;
        let (cols, rows, cw, ch) = (GATE_GRID_COLS, GATE_GRID_ROWS, GATE_CELL_W, GATE_CELL_H);
        let ground = crate::scene::ground_row_for(rows);
        let mut grid = Grid::new(cols, rows, cw, ch);
        for c in 0..cols as i64 {
            grid.set(
                c,
                ground,
                crate::scene::grass_rail_char(c),
                crate::scene::GRASS_COLOR,
            );
            grid.set(
                c,
                ground + 1,
                crate::scene::grass_wave_char(c),
                crate::scene::GRASS_COLOR,
            );
        }
        let big = crate::sprites::big_duck();
        let small = crate::sprites::small_duck();
        let bottom = ground - 1; // feet touch the rail row
        let big_cols = [40i64, 260];
        let small_cols = [520i64, 660, 800];
        for (i, &c) in big_cols.iter().enumerate() {
            big.draw(
                &mut grid,
                c,
                bottom - big.rows as i64 + 1,
                i * 2,
                crate::scene::DUCK_COLOR,
            );
        }
        for (i, &c) in small_cols.iter().enumerate() {
            small.draw(
                &mut grid,
                c,
                bottom - small.rows as i64 + 1,
                i,
                crate::scene::DUCK_COLOR,
            );
        }
        // Every duck plants at least one foot on the row above the rail and
        // stays fully inside the grid vertically.
        for (sprite, cols_list) in [(&big, &big_cols[..]), (&small, &small_cols[..])] {
            assert_ducks_planted(&grid, sprite, cols_list, bottom);
        }
        let mut font = Font::new(cw, ch).expect("font at 2x3");
        let mut fb = FrameBuffer::new(cols as u32 * cw, rows as u32 * ch);
        fb.clear_black();
        blit_grid_rows(&grid, &mut font, &mut fb, 0, &[]);
        // Temp dir, not REVIEW/: tests must not write into the repo.
        let dir = unique_dir("fix2-proof");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fix2_sprite_proof.png");
        save_png(&fb, &path).expect("write fix2 proof png");
        println!("wrote {}", path.display());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every duck in `cols_list` plants a foot on the rail row (audit
    /// nesting: the column loop lives here so the caller reads flat).
    fn assert_ducks_planted(
        grid: &crate::grid::Grid,
        sprite: &crate::sprites::WalkSprite,
        cols_list: &[i64],
        bottom: i64,
    ) {
        for &c in cols_list {
            assert_duck_planted(grid, sprite, c, bottom);
        }
    }

    /// One duck at column `c` plants a foot on the rail row: a leg-column
    /// sample near the front or back third carries `X` at `bottom`.
    fn assert_duck_planted(
        grid: &crate::grid::Grid,
        sprite: &crate::sprites::WalkSprite,
        c: i64,
        bottom: i64,
    ) {
        let front = sprite.cols * crate::sprites::LEG_FRONT_PERCENT / 100;
        let back = sprite.cols * crate::sprites::LEG_BACK_PERCENT / 100;
        let planted = [front, front.saturating_sub(2), back, back + 2]
            .iter()
            .any(|&l| foot_hits(grid, c, l, bottom));
        assert!(planted, "duck at {c} must plant a foot on the rail row");
    }

    /// True when the leg column `c + leg` carries duck ink at `bottom`.
    fn foot_hits(grid: &crate::grid::Grid, c: i64, leg: usize, bottom: i64) -> bool {
        grid.char_at((c + leg as i64) as usize, bottom as usize) == Some('X')
    }
}

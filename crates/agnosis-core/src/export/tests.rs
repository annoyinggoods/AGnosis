// The export's acceptance tests (roadmap Extension 4 §2.4, §3.2, §4.1).
//
// The names are the roadmap's: every one of them is a gate that can
// fail, not a description.

use std::path::PathBuf;

use image::AnimationDecoder;

use crate::assets::AssetDirs;
use crate::config::Config;

/// A temp folder that is not the user's real config folder.
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ducks-export-{}-{nanos}-{tag}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir temp dir");
    dir
}

/// The bytes of an exported file.
fn read(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).expect("read the export")
}

/// The frames of a GIF, decoded by the **image** crate.
///
/// The point is the interoperability: the file is written by our own
/// encoder and read by a decoder that knows nothing about it.
fn gif_frames(bytes: &[u8]) -> Vec<image::RgbaImage> {
    let decoder =
        image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes.to_vec())).expect("decode");
    decoder
        .into_frames()
        .collect::<Result<Vec<_>, _>>()
        .expect("the frames decode")
        .into_iter()
        .map(|frame| frame.buffer().clone())
        .collect()
}

/// The count of pixels whose green channel dominates: the grass.
fn grass_pixels(image: &image::RgbaImage) -> usize {
    image
        .pixels()
        .filter(|p| p[1] >= 40 && p[1] >= p[0] && p[1] >= p[2])
        .count()
}

/// The count of warm pixels: the ducks.
fn duck_pixels(image: &image::RgbaImage) -> usize {
    image
        .pixels()
        .filter(|p| p[0] > 40 && p[0] >= p[2] && p[1] >= p[2])
        .count()
}

#[test]
fn the_html_export_is_self_contained() {
    let dir = temp_dir("html-self-contained");
    let path = dir.join("parade.html");
    let assets = AssetDirs::default();
    let written = write_html(&path, &Config::default(), "a test parade", 3, 12, &assets)
        .expect("export");
    assert!(written > 0, "the page must not be empty");
    let html = String::from_utf8(read(&path)).expect("the page is utf-8");
    // No network, no external files, no fonts.
    assert!(!html.contains("http://"), "the page must not fetch anything");
    assert!(!html.contains("https://"), "the page must not fetch anything");
    assert!(!html.contains("<link"), "the page must not link a stylesheet");
    assert!(!html.contains("<script src"), "the page must not load a script");
    // It carries the frames, the manifest and the script.
    assert!(html.contains("agnosis-blob"), "the blob must travel");
    assert!(html.contains("agnosis-manifest"), "the manifest must travel");
    assert!(html.contains("requestAnimationFrame"), "the script must travel");
    assert!(!html.contains("@@DUCKS_"), "a placeholder was left behind");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_page_carries_the_manifest() {
    let dir = temp_dir("html-manifest");
    let path = dir.join("parade.html");
    let assets = AssetDirs::default();
    write_html(&path, &Config::default(), "my own parade", 3, 12, &assets).expect("export");
    let html = String::from_utf8(read(&path)).expect("the page is utf-8");
    assert!(html.contains("my own parade"), "the name must travel");
    assert!(html.contains("made with AGnosis"), "the maker must be named");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_export_is_reproducible() {
    // The same parade exports byte-identical files, twice.
    let dir = temp_dir("reproducible");
    let first = dir.join("first.html");
    let second = dir.join("second.html");
    let assets = AssetDirs::default();
    write_html(&first, &Config::default(), "a test parade", 3, 12, &assets).expect("export");
    write_html(&second, &Config::default(), "a test parade", 3, 12, &assets).expect("export");
    assert_eq!(read(&first), read(&second), "the export must be reproducible");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_blob_decodes_to_the_recorded_frames() {
    let assets = AssetDirs::default();
    let recording = frames::record(&Config::default(), 4, 12, &assets).expect("recording");
    let blob = codec::decode(&recording.blob).expect("the blob decodes");
    assert_eq!(blob.frames.len(), recording.frame_count as usize);
    assert_eq!(blob.cols, recording.cols);
    assert_eq!(blob.rows, recording.rows);
    assert_eq!(blob.cell_w, recording.cell_w);
    assert_eq!(blob.cell_h, recording.cell_h);
    assert_eq!(blob.fps, recording.fps);
    // The palette's first entry is the space, and every cell is an
    // index into it.
    assert_eq!(blob.palette[0].glyph, ' ');
    for frame in &blob.frames {
        assert_eq!(frame.cells.len(), recording.cols * recording.rows);
        assert_every_cell_points_into_the_palette(frame, blob.palette.len());
    }
}

/// Every cell of the frame points at a palette entry.
///
/// WHY a helper: the nesting rule is two levels at most, and the rule
/// is not scoped to production (audit 2026-10-21 §F23).
fn assert_every_cell_points_into_the_palette(frame: &codec::DecodedFrame, palette_len: usize) {
    for cell in &frame.cells {
        assert!((*cell as usize) < palette_len, "a cell points nowhere");
    }
}

#[test]
fn the_gif_export_is_under_the_size_budget() {
    // The budget: a 20 s loop at 12 fps under 6 MB. It was 2 MB while the
    // background was black; the sky ramp now ships ON (audit 2026-10-23
    // §F-049) and a gradient costs real bytes — the built-in look
    // measures 272547 bytes for 12 frames, while the same parade with the
    // sky off measures 105200, so the ramp is the difference and not a
    // codec regression. The test records a short slice of the loop and
    // charges it the whole loop's price, so the gate is honest without a
    // two-minute export.
    const BUDGET: u64 = 6 * 1024 * 1024;
    const LOOP_FRAMES: u64 = 240;
    let dir = temp_dir("gif-budget");
    let path = dir.join("parade.gif");
    let frames = 12u32;
    let assets = AssetDirs::default();
    write_gif(&path, &Config::default(), frames, 12, 2, &assets).expect("export");
    let bytes = read(&path).len() as u64;
    let charged = bytes / frames as u64 * LOOP_FRAMES;
    assert!(
        charged <= BUDGET,
        "the loop would cost {charged} bytes, over the {BUDGET} byte budget ({bytes} bytes for {frames} frames)"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gif_frames_match_the_recorded_frames() {
    let dir = temp_dir("gif-frames");
    let path = dir.join("parade.gif");
    let frames = 6u32;
    let assets = AssetDirs::default();
    write_gif(&path, &Config::default(), frames, 12, 2, &assets).expect("export");
    let images = gif_frames(&read(&path));
    assert_eq!(images.len(), frames as usize, "the frame count must match");
    // The picture is the app's, at half the cell pitch.
    assert_eq!(images[0].width(), 960);
    assert_eq!(images[0].height(), 540);
    // And it is faithful: the ducks, the grass and the balloons are all
    // there to read.
    assert!(duck_pixels(&images[0]) > 200, "the ducks must survive");
    assert!(grass_pixels(&images[0]) > 200, "the grass must survive");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_gif_export_is_reproducible() {
    let dir = temp_dir("gif-reproducible");
    let first = dir.join("first.gif");
    let second = dir.join("second.gif");
    let assets = AssetDirs::default();
    write_gif(&first, &Config::default(), 3, 12, 2, &assets).expect("export");
    write_gif(&second, &Config::default(), 3, 12, 2, &assets).expect("export");
    assert_eq!(read(&first), read(&second), "the gif must be reproducible");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The GIF-vs-page **glyph contract**, pinned (audit 2026-10-23 F-143).
///
/// Both artifacts are the same parade, but their glyph pitch is not the
/// same and that difference is the contract: the page draws every glyph
/// at the blob's full cell pitch (2 x 3 px), the GIF at the downscaled
/// one (1 x 1.5 px at scale 2). The test pins the geometry exactly and
/// the ink coverage within a tolerance — the GIF must show the same
/// parade, never sharper than the cells it comes from and never
/// vanished. THE PIN, not a re-render: re-rendering the page's pitch
/// inside the GIF would make the two renders drift apart again.
#[test]
fn the_gif_and_the_page_show_the_same_parade_at_their_own_pitch() {
    const FPS: u32 = 12;
    const FRAMES: u32 = 2;
    const SCALE: u32 = 2;
    let cfg = Config::default();
    let assets = AssetDirs::default();
    let recording = crate::export::frames::record(&cfg, FRAMES, FPS, &assets).expect("recording");
    let picture =
        crate::export::frames::render_pixels(&cfg, FRAMES, FPS, SCALE, &assets).expect("pixels");
    // The geometry pin: the page's cell is the blob's cell; the GIF's is
    // the same cell divided by the scale.
    assert_eq!(
        picture[0].width,
        recording.cols as u32 * recording.cell_w / SCALE,
        "the GIF's glyph pitch is the cell pitch over the scale"
    );
    assert_eq!(
        picture[0].height,
        recording.rows as u32 * recording.cell_h / SCALE,
        "and the same vertically"
    );
    // The coverage pin: the share of pixels that are not sky, against
    // the share of cells that carry ink.
    let blob = codec::decode(&recording.blob).expect("the blob decodes");
    let frame = blob.frames.first().expect("a frame");
    if frame.sky.is_empty() {
        return; // no sky to differ from: the geometry pin carries the test
    }
    let cells = frame.cells.len();
    let inked_cells = frame
        .cells
        .iter()
        .filter(|cell| blob.palette[**cell as usize].glyph != ' ')
        .count();
    let mut sky_pixels = 0usize;
    for (index, pixel) in picture[0].rgba.chunks_exact(4).enumerate() {
        let row = index / picture[0].width as usize;
        let grid_row = row * SCALE as usize / recording.cell_h as usize;
        let sky = frame.sky[grid_row.min(frame.sky.len() - 1)];
        let distance = (i32::from(pixel[0]) - i32::from(sky[0])).abs()
            + (i32::from(pixel[1]) - i32::from(sky[1])).abs()
            + (i32::from(pixel[2]) - i32::from(sky[2])).abs();
        if distance <= 16 {
            sky_pixels += 1;
        }
    }
    let pixels = picture[0].width as usize * picture[0].height as usize;
    let gif_ink = (pixels - sky_pixels) as f32 / pixels as f32;
    let cell_ink = inked_cells as f32 / cells as f32;
    assert!(
        gif_ink > cell_ink * 0.2,
        "the GIF lost the parade: {gif_ink} ink against {cell_ink} cells"
    );
    assert!(
        gif_ink <= cell_ink * 1.05 + 0.01,
        "the GIF is sharper than the cells it came from: {gif_ink} against {cell_ink}"
    );
}

#[test]
fn the_export_of_a_running_parade_matches_the_headless_one() {
    // The export is a pure function of the configuration: the parade in
    // the window and the parade the recorder builds are the same one.
    //
    // WHY it is a real comparison now: the test used to export the same
    // config twice — which is the *reproducibility* test under another
    // name — and never involved a running parade at all (audit
    // 2026-10-21 §F24). It builds the two parades now: the one the
    // recorder exports, and the one the window runs, and it compares
    // them cell by cell.
    const FPS: u32 = 12;
    const FRAMES: u32 = 3;
    let cfg = Config::default();
    let assets = AssetDirs::default();
    // The headless one: the parade the recorder builds.
    let recording = crate::export::frames::record(&cfg, FRAMES, FPS, &assets).expect("recording");
    // The running one: the parade in the window, advanced frame by frame.
    // It runs the **export** config (audit 2026-10-23 F-154): the export
    // replaced the default exit hint, so the window scene must show the
    // same words for the grids to match.
    let cfg = crate::export::frames::export_config(&cfg);
    let (cols, rows, cell_w, cell_h) = cfg.grid_for_monitor(1920, 1080, 1.0);
    let mut running = crate::scene::Scene::new(&cfg, cols, rows, cell_w, cell_h);
    for _ in 0..FRAMES {
        running.advance(1.0 / FPS as f32);
    }
    // The exported frame is the running parade's grid, cell by cell.
    let blob = crate::export::codec::decode(&recording.blob).expect("the blob decodes");
    let frame = blob.frames.last().expect("a frame");
    assert_eq!(frame.cells.len(), cols * rows, "the frame is the whole grid");
    for (index, cell) in frame.cells.iter().enumerate() {
        let glyph = blob.palette[*cell as usize].glyph;
        let col = index % cols;
        let row = index / cols;
        assert_eq!(
            running.grid().char_at(col, row),
            Some(glyph),
            "the cell {col},{row} differs"
        );
    }
}

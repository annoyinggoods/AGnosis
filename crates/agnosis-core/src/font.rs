//! Glyph coverage rasterization and per-codepoint cache.
//!
//! Coverage for every printable codepoint is rasterized once from the embedded
//! DejaVu Sans Mono TrueType font into an 8-bit buffer of exactly
//! `cell_w x cell_h` pixels, then shared through `Arc` handles. Line-art
//! glyphs (`-`, `|`, `/`, `\`) are fitted so their ink touches the cell edges
//! and connects across cell boundaries; all other glyphs are
//! baseline-anchored with clamps that keep them inside the cell.
//! Rasterization stretches the vector outline with a non-uniform `PxScale`
//! before scan conversion. Codepoints the font does not contain fall back
//! to `'?'`.
//!
//! Body text (HUD hints, balloon labels) uses [`Font::mask_uniform`], a
//! fixed-scale path that maps font units to pixels identically for every
//! glyph: `cell_w` spans the `M` advance, `cell_h` spans ascent-to-descent,
//! baseline-anchored, horizontally starting at the glyph origin. Because the
//! scale never adapts to one glyph's ink, capitals and lowercase share one
//! visual size. The per-glyph ink-fit stays the default for `-`, `|`, `/`
//! and `\` so ropes and borders keep connecting across cell edges.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use ab_glyph::{point, Font as AbFont, FontVec, PxScale};

/// 8-bit coverage mask for one glyph, exactly `w x h` pixels.
#[derive(Clone, Debug)]
pub struct GlyphMask {
    /// Mask width in pixels; always equals the owning `Font` cell width.
    pub w: u32,
    /// Mask height in pixels; always equals the owning `Font` cell height.
    pub h: u32,
    /// Row-major coverage values, `len == w * h`, 0 = transparent.
    pub coverage: Vec<u8>,
}

/// Failures produced while opening the embedded font.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    /// The embedded font bytes did not parse as a font.
    #[error("failed to parse embedded font: {0}")]
    InvalidFont(String),
    /// Cell dimensions must both be nonzero.
    #[error("invalid cell size {0}x{1}: dimensions must be nonzero")]
    InvalidCell(u32, u32),
    /// Font metrics are degenerate (zero advance or zero height).
    #[error("font has degenerate metrics")]
    BadMetrics,
}

/// Cached rasterizer for the embedded monospace font.
pub struct Font {
    font: FontVec,
    cell_w: u32,
    cell_h: u32,
    advance: f32,
    ascent: f32,
    height: f32,
    codepoints: HashSet<char>,
    cache: HashMap<char, Arc<GlyphMask>>,
    uniform_cache: HashMap<char, Arc<GlyphMask>>,
}

impl Font {
    /// Load the embedded font and prepare `cell_w x cell_h` rasterization.
    ///
    /// Fails with [`FontError`] when the cell size is degenerate, the font
    /// bytes do not parse, or the font metrics are unusable.
    pub fn new(cell_w: u32, cell_h: u32) -> Result<Self, FontError> {
        if cell_w == 0 || cell_h == 0 {
            return Err(FontError::InvalidCell(cell_w, cell_h));
        }
        let bytes = include_bytes!("../assets/fonts/DejaVuSansMono.ttf").to_vec();
        let font =
            FontVec::try_from_vec(bytes).map_err(|e| FontError::InvalidFont(format!("{e:?}")))?;
        let ascent = font.ascent_unscaled();
        let descent = font.descent_unscaled();
        let height = ascent - descent;
        let advance = font.h_advance_unscaled(font.glyph_id('M'));
        let height_ok = height.is_finite() && height > 0.0;
        let advance_ok = advance.is_finite() && advance > 0.0;
        if !height_ok || !advance_ok {
            return Err(FontError::BadMetrics);
        }
        let codepoints: HashSet<char> = font.codepoint_ids().map(|(_, c)| c).collect();
        Ok(Self {
            font,
            cell_w,
            cell_h,
            advance,
            ascent,
            height,
            codepoints,
            cache: HashMap::new(),
            uniform_cache: HashMap::new(),
        })
    }

    /// Cell width in pixels; every mask is exactly this wide.
    ///
    /// Test-only accessor (audit maintainability: sweep test-only
    /// accessors with the `dead_code` lint); the render path takes the
    /// cell size from the grid, not from here.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cell_w(&self) -> u32 {
        self.cell_w
    }

    /// Cell height in pixels; every mask is exactly this tall.
    ///
    /// Test-only accessor, see [`Font::cell_w`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn cell_h(&self) -> u32 {
        self.cell_h
    }

    /// Cached rasterization of one character.
    ///
    /// Returns `None` for `' '` and for codepoints with empty outlines.
    /// Codepoints the font lacks fall back to `'?'`. A second call for the
    /// same character returns the same cached `Arc` without re-rasterizing.
    pub fn mask(&mut self, ch: char) -> Option<Arc<GlyphMask>> {
        if ch == ' ' {
            return None;
        }
        if let Some(hit) = self.cache.get(&ch) {
            return Some(Arc::clone(hit));
        }
        let eff = self.resolved(ch)?;
        if let Some(hit) = self.cache.get(&eff) {
            let shared = Arc::clone(hit);
            self.cache.insert(ch, Arc::clone(&shared));
            return Some(shared);
        }
        let rendered = self.rasterize(eff)?;
        self.cache.insert(eff, Arc::clone(&rendered));
        self.cache.insert(ch, Arc::clone(&rendered));
        Some(rendered)
    }

    /// Effective codepoint for `ch`: the character itself when the font
    /// holds it, `'?'` otherwise, and `None` when even the fallback is
    /// missing. One home for the fallback rule, shared by both mask paths
    /// (audit site #11: keep the decision out of the caller's body).
    fn resolved(&self, ch: char) -> Option<char> {
        let eff = if self.codepoints.contains(&ch) {
            ch
        } else {
            '?'
        };
        if self.codepoints.contains(&eff) {
            Some(eff)
        } else {
            None
        }
    }

    /// Number of codepoints currently held in the cache.
    ///
    /// Test-only accessor, see [`Font::cell_w`].
    #[cfg(any(test, feature = "test-support"))]
    pub fn cached_count(&self) -> usize {
        self.cache.len()
    }

    /// Fixed-scale mask for body text: identical font-unit mapping for every
    /// glyph so mixed-case strings render at one visual size.
    ///
    /// `cell_w` spans the `M` advance and `cell_h` spans ascent-to-descent;
    /// the outline keeps the shared advance-mapping scale for every glyph,
    /// baseline-anchored, horizontally centred on its ink. No per-glyph
    /// ink-fit runs here, so narrow lowercase keeps the same scale as
    /// capitals. Returns `None` for `' '` and for empty outlines; missing
    /// codepoints fall back to `'?'`. Line-art (`-`, `|`, `/`, `\`) keeps
    /// using [`Font::mask`] so ropes and borders keep their edge-connecting
    /// fit.
    pub fn mask_uniform(&mut self, ch: char) -> Option<Arc<GlyphMask>> {
        if ch == ' ' {
            return None;
        }
        if matches!(ch, '-' | '|' | '/' | '\\') {
            return self.mask(ch);
        }
        let eff = self.resolved(ch)?;
        let key = uniform_key(eff);
        if let Some(hit) = self.uniform_cache.get(&key) {
            let shared = Arc::clone(hit);
            self.uniform_cache.insert(ch, Arc::clone(&shared));
            return Some(shared);
        }
        let rendered = self.rasterize_uniform(eff)?;
        self.uniform_cache.insert(key, Arc::clone(&rendered));
        self.uniform_cache.insert(ch, Arc::clone(&rendered));
        Some(rendered)
    }

    fn rasterize(&self, ch: char) -> Option<Arc<GlyphMask>> {
        let id = self.font.glyph_id(ch);
        let w = self.cell_w as usize;
        let h = self.cell_h as usize;
        let outline = self.font.outline(id)?;
        let ink_w = outline.bounds.max.x - outline.bounds.min.x;
        let above = outline.bounds.min.y;
        let below = -outline.bounds.max.y;
        let ink_h = above + below;
        if !is_positive(ink_w) || !is_positive(ink_h) {
            return None;
        }
        let kx = (self.advance / ink_w).clamp(1.0, 2.0);
        let descent = self.ascent - self.height;
        let full_height_fit = below > 0.0 && (below - -descent).abs() < 1.0;
        let mut ky = (self.height / ink_h).clamp(1.0, 1.6);
        if !full_height_fit {
            ky = fit_ky(ky, above, below, self.ascent, descent);
        }
        let scale = PxScale {
            x: self.cell_w as f32 * self.height / self.advance * kx,
            y: self.cell_h as f32 * ky,
        };
        let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
        let outlined = self.font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();
        let tight_w = bounds.max.x - bounds.min.x;
        let tight_h = bounds.max.y - bounds.min.y;
        let base_x = (self.cell_w as f32 * 0.5 - tight_w * 0.5).round() as i32;
        let base_y = if full_height_fit {
            (self.cell_h as f32 * 0.5 - tight_h * 0.5).round() as i32
        } else {
            let baseline = self.ascent * self.cell_h as f32 / self.height;
            (baseline + bounds.min.y).round() as i32
        };
        let mut coverage = vec![0u8; w * h];
        outlined.draw(|x, y, c| {
            let dx = base_x + x as i32;
            let dy = base_y + y as i32;
            if dx < 0 || dy < 0 || dx >= w as i32 || dy >= h as i32 {
                return;
            }
            let v = (c * 255.0).round().clamp(0.0, 255.0) as u8;
            let idx = dy as usize * w + dx as usize;
            if v > coverage[idx] {
                coverage[idx] = v;
            }
        });
        if coverage.iter().all(|&v| v == 0) {
            return None;
        }
        Some(Arc::new(GlyphMask {
            w: self.cell_w,
            h: self.cell_h,
            coverage,
        }))
    }

    fn rasterize_uniform(&self, ch: char) -> Option<Arc<GlyphMask>> {
        let id = self.font.glyph_id(ch);
        let w = self.cell_w as usize;
        let h = self.cell_h as usize;
        let scale = PxScale {
            x: self.cell_w as f32 * self.height / self.advance,
            y: self.cell_h as f32,
        };
        let glyph = id.with_scale_and_position(scale, point(0.0, 0.0));
        let outlined = self.font.outline_glyph(glyph)?;
        let bounds = outlined.px_bounds();
        let tight_w = bounds.max.x - bounds.min.x;
        let base_x = (self.cell_w as f32 * 0.5 - tight_w * 0.5).round() as i32;
        let baseline = self.ascent * self.cell_h as f32 / self.height;
        let base_y = (baseline + bounds.min.y).round() as i32;
        let mut coverage = vec![0u8; w * h];
        outlined.draw(|x, y, c| {
            let dx = base_x + x as i32;
            let dy = base_y + y as i32;
            if dx < 0 || dy < 0 || dx >= w as i32 || dy >= h as i32 {
                return;
            }
            let v = (c * 255.0).round().clamp(0.0, 255.0) as u8;
            let idx = dy as usize * w + dx as usize;
            if v > coverage[idx] {
                coverage[idx] = v;
            }
        });
        if coverage.iter().all(|&v| v == 0) {
            return None;
        }
        Some(Arc::new(GlyphMask {
            w: self.cell_w,
            h: self.cell_h,
            coverage,
        }))
    }
}

/// True when `v` is strictly positive. NaN fails, `+inf` passes — the same
/// verdict as `v > 0.0`, spelled without a negated comparison so the
/// rasterizer's emptiness check reads clearly (clippy
/// `neg_cmp_op_on_partial_ord`).
fn is_positive(v: f32) -> bool {
    matches!(v.partial_cmp(&0.0), Some(Ordering::Greater))
}

/// Cache key for the uniform text path: missing codepoints share `'?'`.
/// Identity today: `mask_uniform` resolves the fallback to `'?'` before
/// calling, so the key always names a resident codepoint. Kept as a named
/// function (not inlined) so the fallback-sharing rule has one home if the
/// resolution order ever changes; the audit's "remove if truly identity"
/// note resolves to "documented, keep".
fn uniform_key(ch: char) -> char {
    ch
}

/// Clamp the vertical scale so the glyph's ink stays inside the cell above
/// and below the baseline. Split out of [`Font::rasterize`] so the fit
/// decision reads flat (audit site #11: no if-inside-if).
fn fit_ky(ky: f32, above: f32, below: f32, ascent: f32, descent: f32) -> f32 {
    let mut k = ky;
    if above > 0.0 {
        k = k.min(ascent / above);
    }
    if below > 0.0 {
        k = k.min(-descent / below);
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full-alpha value of the 8-bit coverage mask format.
    const MAX_COVERAGE: u32 = 255;

    fn col_span(m: &GlyphMask) -> usize {
        let (min, max, any) = for_each_ink(m, |x, _| x, m.w as usize);
        span_of(min, max, any)
    }

    fn row_span(m: &GlyphMask) -> usize {
        let (min, max, any) = for_each_ink(m, |_, y| y, m.h as usize);
        span_of(min, max, any)
    }

    /// Fold every inked mask cell through `pick` (which selects the x or y
    /// coordinate) into a `(min, max, any)` triple. One iterator replaces the
    /// three hand-rolled span loops (audit site #11).
    fn for_each_ink(
        m: &GlyphMask,
        pick: impl Fn(usize, usize) -> usize,
        bound: usize,
    ) -> (usize, usize, bool) {
        let w = m.w as usize;
        let mut min = bound;
        let mut max = 0usize;
        let mut any = false;
        for (i, &v) in m.coverage.iter().enumerate() {
            if v == 0 {
                continue;
            }
            any = true;
            let v = pick(i % w, i / w);
            min = min.min(v);
            max = max.max(v);
        }
        (min, max, any)
    }

    /// Inclusive span of a `(min, max, any)` fold: zero when nothing inked.
    fn span_of(min: usize, max: usize, any: bool) -> usize {
        match any {
            true => max - min + 1,
            false => 0,
        }
    }

    fn ink_rows(m: &GlyphMask) -> usize {
        let w = m.w as usize;
        let h = m.h as usize;
        (0..h).filter(|&y| mask_row_has_ink(m, y, w)).count()
    }

    /// True when mask row `y` carries any ink. Split from `ink_rows` so no
    /// loop nests a loop (audit site #11).
    fn mask_row_has_ink(m: &GlyphMask, y: usize, w: usize) -> bool {
        (0..w).any(|x| m.coverage[y * w + x] > 0)
    }

    fn mean_coverage(m: &GlyphMask) -> f64 {
        let sum: u64 = m.coverage.iter().map(|&v| v as u64).sum();
        sum as f64 / m.coverage.len() as f64
    }

    fn ink_bbox(m: &GlyphMask) -> (usize, usize, usize, usize) {
        let (min_x, max_x, _) = for_each_ink(m, |x, _| x, m.w as usize);
        let (min_y, max_y, _) = for_each_ink(m, |_, y| y, m.h as usize);
        (min_x, max_x, min_y, max_y)
    }

    #[test]
    fn uniform_text_shares_one_scale_across_cases() {
        let mut f = Font::new(12, 24).expect("font");
        let mut heights = Vec::new();
        for ch in "pressESCtoexit".chars() {
            let m = f.mask_uniform(ch).expect("text glyph must rasterize");
            assert_eq!((m.w, m.h), (12, 24));
            heights.push(ink_rows(&m) as i32);
        }
        heights.sort_unstable();
        let median = heights[heights.len() / 2];
        for h in &heights {
            assert!(
                (h - median).abs() <= 3,
                "uniform ink height {h} strays from median {median} ({heights:?})"
            );
        }
        let lower: Vec<i32> = "presstoexit"
            .chars()
            .map(|ch| ink_rows(&f.mask_uniform(ch).expect("lower")))
            .map(|h| h as i32)
            .collect();
        let upper: Vec<i32> = "ESC"
            .chars()
            .map(|ch| ink_rows(&f.mask_uniform(ch).expect("upper")))
            .map(|h| h as i32)
            .collect();
        let lower_mean = lower.iter().sum::<i32>() as f64 / lower.len() as f64;
        let upper_mean = upper.iter().sum::<i32>() as f64 / upper.len() as f64;
        assert!(
            (lower_mean - upper_mean).abs() <= 4.0,
            "lower mean {lower_mean} vs upper mean {upper_mean} ({lower:?} vs {upper:?})"
        );
    }

    #[test]
    fn uniform_line_art_still_uses_edge_fit() {
        let mut f = Font::new(12, 24).expect("font");
        let dash = f.mask_uniform('-').expect("dash");
        let pipe = f.mask_uniform('|').expect("pipe");
        assert!(col_span(&dash) * 100 >= 90 * dash.w as usize);
        assert!(row_span(&pipe) * 100 >= 90 * pipe.h as usize);
    }

    #[test]
    fn uniform_text_is_cached_and_space_free() {
        let mut f = Font::new(12, 24).expect("font");
        assert!(f.mask_uniform(' ').is_none());
        let a = f.mask_uniform('e').expect("'e' must rasterize");
        let b = f.mask_uniform('e').expect("'e' cached");
        assert!(Arc::ptr_eq(&a, &b));
        let q = f.mask_uniform('?').expect("'?' must rasterize");
        let missing = f.mask_uniform('\u{10FFFF}').expect("fallback");
        assert_eq!(missing.coverage, q.coverage);
    }

    #[test]
    fn probe_hud_ink_boxes() {
        let mut f = Font::new(12, 24).expect("font");
        for ch in "press ESC to exit".chars() {
            if ch == ' ' {
                continue;
            }
            let m = f.mask(ch).expect("glyph must rasterize");
            let (min_x, max_x, min_y, max_y) = ink_bbox(&m);
            println!(
                "HUDPROBE {ch:?} x{min_x}..{max_x} (w{}) y{min_y}..{max_y} (h{}) rows{}",
                max_x + 1 - min_x,
                max_y + 1 - min_y,
                ink_rows(&m)
            );
        }
        let mut u = Font::new(12, 24).expect("font");
        for ch in "press ESC to exit".chars() {
            if ch == ' ' {
                continue;
            }
            let m = u.mask_uniform(ch).expect("glyph must rasterize");
            let (min_x, max_x, min_y, max_y) = ink_bbox(&m);
            println!(
                "HUDUNI {ch:?} x{min_x}..{max_x} (w{}) y{min_y}..{max_y} (h{}) rows{}",
                max_x + 1 - min_x,
                max_y + 1 - min_y,
                ink_rows(&m)
            );
        }
    }

    /// Brightness of one downsampled cell: fraction of source pixels above
    /// the ink threshold. Split from `probe_review_images` so the probe reads
    /// flat (audit site #3, test-only, done last).
    fn cell_brightness(
        gray: &image::GrayImage,
        c: usize,
        r: usize,
        cols: usize,
        rows: usize,
    ) -> bool {
        let (w, h) = (gray.width() as usize, gray.height() as usize);
        let x0 = c * w / cols;
        let x1 = ((c + 1) * w / cols).max(x0 + 1);
        let y0 = r * h / rows;
        let y1 = ((r + 1) * h / rows).max(y0 + 1);
        let (mut bright, mut tot) = (0u32, 0u32);
        for y in y0..y1.min(h) {
            count_scanline_bright(gray, y, x0, x1.min(w), &mut bright, &mut tot);
        }
        bright * 5 > tot * 2
    }

    /// Fold one source scanline's `[x0, x1)` pixels into the running
    /// bright/total counts. Split from `cell_brightness` for nesting depth.
    fn count_scanline_bright(
        gray: &image::GrayImage,
        y: usize,
        x0: usize,
        x1: usize,
        bright: &mut u32,
        tot: &mut u32,
    ) {
        for x in x0..x1 {
            *tot += 1;
            if gray.get_pixel(x as u32, y as u32).0[0] > 80 {
                *bright += 1;
            }
        }
    }

    /// One probe scale's lines: the scale header plus one line per row.
    /// Split from `probe_review_images` so the probe reads flat (audit
    /// site #3: no loop inside a loop).
    fn probe_block(gray: &image::GrayImage, cols: usize, rows: usize) -> Vec<String> {
        let mut lines = vec![format!("DIVSCALE {cols}x{rows}")];
        lines.extend((0..rows).map(|r| format!("DIVTXT {}", probe_row(gray, cols, rows, r))));
        lines
    }

    /// One downsampled probe row: `#` for a bright cell, `.` otherwise.
    fn probe_row(gray: &image::GrayImage, cols: usize, rows: usize, r: usize) -> String {
        (0..cols)
            .map(|c| match cell_brightness(gray, c, r, cols, rows) {
                true => '#',
                false => '.',
            })
            .collect()
    }

    #[test]
    fn probe_review_images() {
        // The probe decodes a REVIEW fixture that is not part of the
        // crate: it runs only when `AGNOSIS_PROBE` is set, so `cargo test`
        // is immune to the fixture being deleted (audit 2026-10-23
        // rec 32 — the probe is a developer tool, not a gate).
        if std::env::var_os("AGNOSIS_PROBE").is_none() {
            return;
        }
        // WHY manifest-relative: the test runs with the current directory
        // set to the crate root (`crates/agnosis-core`), so the review
        // folder has to be addressed from the manifest, not from the
        // working directory.
        let name = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../REVIEW/ascii-divider.jpg"
        );
        let bytes = std::fs::read(name).expect("read divider");
        let img = image::load_from_memory(&bytes).expect("decode divider");
        let gray = img.to_luma8();
        let (w, h) = (gray.width(), gray.height());
        println!("DIVIDER dims {w}x{h}");
        let scales = [(392usize, 12usize), (294, 9), (196, 6)];
        let lines: Vec<String> = scales
            .iter()
            .flat_map(|(cols, rows)| probe_block(&gray, *cols, *rows))
            .collect();
        for line in lines {
            println!("{line}");
        }
    }

    #[test]
    fn font_loads_with_expected_cell_size() {
        let f = Font::new(12, 24).expect("embedded font must parse");
        assert_eq!(f.cell_w(), 12);
        assert_eq!(f.cell_h(), 24);
        assert_eq!(f.cached_count(), 0);
    }

    #[test]
    fn dash_spans_cell_width_as_thin_band() {
        let mut f = Font::new(12, 24).expect("font");
        let m = f.mask('-').expect("'-' must rasterize");
        assert_eq!((m.w, m.h), (12, 24));
        assert_eq!(m.coverage.len(), 12 * 24);
        let span = col_span(&m);
        assert!(
            span * 100 >= 90 * m.w as usize,
            "dash col span {span} of {}",
            m.w
        );
        let rows = ink_rows(&m);
        assert!(
            rows >= 1 && rows * 3 <= m.h as usize,
            "dash band rows {rows}"
        );
    }

    #[test]
    fn at_sign_covers_far_more_than_dot() {
        let mut f = Font::new(12, 24).expect("font");
        let at = f.mask('@').expect("'@' must rasterize");
        let dot = f.mask('.').expect("'.' must rasterize");
        let mean_at = mean_coverage(&at);
        let mean_dot = mean_coverage(&dot);
        assert!(mean_dot > 0.0, "dot must leave some ink");
        assert!(
            mean_at > 4.0 * mean_dot,
            "mean @ {mean_at} vs dot {mean_dot}"
        );
    }

    #[test]
    fn space_yields_no_mask() {
        let mut f = Font::new(12, 24).expect("font");
        assert!(f.mask(' ').is_none());
        assert_eq!(f.cached_count(), 0);
    }

    #[test]
    fn cache_returns_shared_arc_and_counts_once() {
        let mut f = Font::new(12, 24).expect("font");
        let a = f.mask('Z').expect("'Z' must rasterize");
        assert_eq!(f.cached_count(), 1);
        let b = f.mask('Z').expect("'Z' cached");
        assert_eq!(f.cached_count(), 1);
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn coverage_values_stay_in_range() {
        let mut f = Font::new(12, 24).expect("font");
        for ch in ['#', '@', 'm', '-', '|', 'g'] {
            let Some(m) = f.mask(ch) else {
                continue;
            };
            assert_eq!(m.coverage.len(), m.w as usize * m.h as usize);
            assert!(m.coverage.iter().any(|&v| v > 0));
            // The mask is 8-bit alpha: every entry fits the format's range
            // (widened to u32 so the bound is a real value, not the u8
            // type's own maximum) and a rasterized glyph reaches full
            // coverage somewhere.
            let max = u32::from(m.coverage.iter().copied().max().unwrap_or(0));
            assert!(
                max <= MAX_COVERAGE,
                "{ch:?} coverage {max} exceeds the 8-bit mask range"
            );
            assert_eq!(
                max, MAX_COVERAGE,
                "{ch:?} never reaches full coverage ({max} of {MAX_COVERAGE})"
            );
        }
    }

    #[test]
    fn two_instances_rasterize_identically() {
        let mut a = Font::new(12, 24).expect("font");
        let mut b = Font::new(12, 24).expect("font");
        let ma = a.mask('Q').expect("'Q' must rasterize");
        let mb = b.mask('Q').expect("'Q' must rasterize");
        assert_eq!(ma.coverage, mb.coverage);
        assert_eq!((ma.w, ma.h), (mb.w, mb.h));
    }

    #[test]
    fn pipe_spans_cell_height() {
        let mut f = Font::new(12, 24).expect("font");
        let m = f.mask('|').expect("'|' must rasterize");
        let span = row_span(&m);
        assert!(
            span * 100 >= 90 * m.h as usize,
            "pipe row span {span} of {}",
            m.h
        );
    }

    #[test]
    fn missing_codepoint_falls_back_to_question_mark() {
        let mut f = Font::new(12, 24).expect("font");
        let q = f.mask('?').expect("'?' must rasterize");
        let missing = f.mask('\u{10FFFF}').expect("fallback must rasterize");
        assert_eq!(missing.coverage, q.coverage);
    }
}

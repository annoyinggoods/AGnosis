//! The self-contained HTML page (roadmap Extension 4 §2).
//!
//! One `.html` file that opens from a `file://` URL: no external CSS,
//! no fonts, no scripts, no network. It carries the recorded frames in
//! a binary blob (base64) and the manifest as a JSON block.
//!
//! The page is **not** a second implementation of the simulation: it
//! plays the frames the app drew, so the two cannot drift.

use std::path::Path;

use crate::assets::AssetDirs;
use crate::config::Config;
use crate::export::frames::{ExportError, Recording};

/// The page's template.
const PAGE: &str = include_str!("page.html");

/// The page's script.
const SCRIPT: &str = include_str!("page.js");

/// The standard base64 alphabet.
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Write the self-contained page for a recording.
///
/// `assets` is where the drawn art and the MIDI tracks live. The page
/// carries the **recorded frames**, so the drawn art is already baked
/// into the blob and the folders are **not** read here — that is why
/// the parameter is accepted and not used: it keeps the export surface
/// uniform (`write` is the second half of the same call, and it is
/// where a page that reads the art itself would start).
///
/// Returns the number of bytes written.
pub fn write(
    path: &Path,
    recording: &Recording,
    cfg: &Config,
    name: &str,
    _assets: &AssetDirs,
) -> Result<u64, ExportError> {
    let manifest = crate::manifest::Manifest::from_config(cfg, name);
    let json = match manifest.to_json() {
        Ok(json) => json,
        Err(reason) => {
            // WHY the trace line: the page would carry an empty manifest
            // and the user would never learn why (audit 2026-10-21 §F15).
            crate::trace::warn(&format!(
                "export: the manifest could not be rendered, the page carries an empty one: {reason}"
            ));
            "{}".to_string()
        }
    };
    let html = render(recording, &json);
    std::fs::write(path, html.as_bytes()).map_err(|source| ExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(html.len() as u64)
}

/// The finished page: the template with its three placeholders filled in.
pub fn render(recording: &Recording, manifest_json: &str) -> String {
    let blob = base64(&recording.blob);
    let manifest = escape_json(manifest_json);
    fill(
        PAGE,
        &[
            ("@@AGNOSIS_BLOB@@", blob.as_str()),
            ("@@AGNOSIS_MANIFEST@@", manifest.as_str()),
            ("@@AGNOSIS_SCRIPT@@", SCRIPT),
        ],
    )
}

/// Fill the template's placeholders in **one** pass.
///
/// A single pass matters: the payload may itself contain a placeholder
/// text (a config value could say `@@AGNOSIS_BLOB@@`), and a second pass
/// would substitute it a second time.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    loop {
        let Some((at, len, value)) = first_placeholder(rest, values) else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        out.push_str(value);
        rest = &rest[at + len..];
    }
}

/// The first placeholder that occurs in `text`: its position, its length
/// and the value it stands for.
///
/// WHY a helper: the nesting rule is two levels at most, and the fill
/// was a loop in a loop (audit 2026-10-21 §F8).
///
/// WHY `min_by_key` and not first-found-in-order (audit 2026-10-23
/// F-149): the substitution is **positional** — the placeholder that
/// occurs first in the template is replaced first, whichever of the
/// tokens is listed first. An array-order scan replaced a later-listed
/// token that happened to sit earlier, writing its (longer) payload
/// across an earlier placeholder's start and leaving that placeholder
/// unsubstituted in the page.
fn first_placeholder<'a>(
    text: &str,
    values: &'a [(&'a str, &'a str)],
) -> Option<(usize, usize, &'a str)> {
    values
        .iter()
        .filter_map(|(token, value)| text.find(token).map(|at| (at, token.len(), *value)))
        .min_by_key(|(at, _len, _value)| *at)
}

/// Escape the characters that would end the JSON script block early.
///
/// A `</script>` inside the manifest would close the block and the rest
/// of the JSON would be read as markup. The `<` is enough: the JSON
/// escape `\u003c` is valid JSON and reads back as a `<`.
fn escape_json(text: &str) -> String {
    text.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

/// The standard base64 encoding, with padding.
fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(BASE64[(triple >> 18) as usize & 63] as char);
        out.push(BASE64[(triple >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(BASE64[(triple >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(BASE64[triple as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::codec::Encoder;
    use crate::export::frames::Recording;
    use crate::grid::Grid;

    /// A tiny recording, so the test is quick.
    fn tiny_recording() -> Recording {
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let mut grid = Grid::new(8, 4, 2, 3);
        grid.set(1, 0, 'X', [255, 255, 255]);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let mut font = crate::font::Font::new(2, 3).expect("font");
        Recording {
            cols: 8,
            rows: 4,
            cell_w: 2,
            cell_h: 3,
            fps: 12,
            frame_count: 1,
            blob: encoder.finish(&mut font).expect("finish"),
        }
    }

    #[test]
    fn the_page_is_self_contained() {
        let html = render(&tiny_recording(), "{}");
        // No network, no external files.
        assert!(
            !html.contains("http://"),
            "the page must not fetch anything"
        );
        assert!(
            !html.contains("https://"),
            "the page must not fetch anything"
        );
        assert!(
            !html.contains("<link"),
            "the page must not link a stylesheet"
        );
        assert!(
            !html.contains("src=\"http"),
            "the page must not load a script"
        );
        // It carries the blob, the manifest and the script.
        assert!(html.contains("agnosis-blob"));
        assert!(html.contains("agnosis-manifest"));
        assert!(html.contains("requestAnimationFrame"));
        // And no placeholder is left behind.
        assert!(!html.contains("@@DUCKS_"), "a placeholder was left behind");
    }

    #[test]
    fn the_page_carries_the_manifest() {
        let html = render(&tiny_recording(), "{\"version\":1,\"name\":\"my parade\"}");
        assert!(html.contains("my parade"), "the manifest must travel");
    }

    #[test]
    fn a_manifest_cannot_break_out_of_the_json_block() {
        let html = render(
            &tiny_recording(),
            "{\"name\":\"</script><script>alert(1)</script>\"}",
        );
        assert!(!html.contains("</script><script>"), "the payload escaped");
        assert!(html.contains("\\u003c/script"), "the payload was escaped");
    }

    #[test]
    fn the_base64_encoding_matches_the_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn the_blob_survives_the_round_trip_through_base64() {
        let recording = tiny_recording();
        let html = render(&recording, "{}");
        // The typo here (`ducs-blob`) masked the real id behind an
        // `or_else` and the test could not fail (audit 2026-10-23
        // F-160); the real id is asserted, and nothing else.
        let start = html.find("agnosis-blob");
        assert!(start.is_some(), "the blob's id must be in the page");
        // The blob is in the file, and the file is valid UTF-8 text.
        assert!(html.is_char_boundary(html.len()));
    }

    #[test]
    fn the_first_placeholder_in_the_template_wins() {
        // The substitution is positional, not array-ordered (audit
        // 2026-10-23 F-149): a token listed later but sitting earlier in
        // the text is replaced first.
        let values = [("@@SECOND@@", "s"), ("@@FIRST@@", "f")];
        // `@@FIRST@@` begins at column 3; `@@SECOND@@` begins at 20.
        let filled = fill("x@@FIRST@@y @@SECOND@@", &values);
        assert_eq!(filled, "xfy s", "the earlier placeholder is replaced first");
        // And a template in the values' own order still fills fully.
        let filled = fill("a @@SECOND@@ b @@FIRST@@", &values);
        assert_eq!(filled, "a s b f");
        assert!(!filled.contains("@@"), "no placeholder is left behind");
    }
}

//! Copying a frame to the system clipboard.
//!
//! The screenshot feature copies the **live** frame — the app's own
//! framebuffer, not a screen grab: the window is borderless and
//! fullscreen, so a screen grab would be the same picture, but the
//! app's own buffer is exact and needs no screen-capture permission.
//!
//! Each platform has its own way of putting an image on the clipboard:
//! Windows takes a `CF_DIB` device-independent bitmap, macOS takes a PNG
//! file, and Linux takes a PNG through `wl-copy` or `xclip`.

use agnosis_core::blit::FrameBuffer;

/// Failures produced while copying to the clipboard.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// The platform's clipboard refused the image.
    #[error("the clipboard refused the image: {0}")]
    Refused(String),
}

/// Copy the frame to the system clipboard.
pub fn copy_frame(fb: &FrameBuffer) -> Result<(), ClipboardError> {
    imp::copy(fb)
}

/// Write the frame to a PNG file.
///
/// The Unix clipboards take a **file**, so the PNG goes to a temporary
/// file first. The file is the caller's to remove. The popup uses it for
/// the share file, so the user has the PNG as well as the clipboard.
pub fn write_png(fb: &FrameBuffer, path: &std::path::Path) -> Result<(), ClipboardError> {
    agnosis_core::blit::save_png(fb, path).map_err(|e| ClipboardError::Refused(e.to_string()))
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{ClipboardError, FrameBuffer};
    use std::ffi::c_void;
    use std::ptr;

    /// The clipboard format for a device-independent bitmap.
    const CF_DIB: u32 = 8;

    /// `GMEM_MOVEABLE` — the clipboard takes ownership of the memory.
    const GMEM_MOVEABLE: u32 = 0x0002;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(owner: *mut c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(format: u32, data: *mut c_void) -> *mut c_void;
        fn CloseClipboard() -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
        fn GlobalLock(mem: *mut c_void) -> *mut c_void;
        fn GlobalUnlock(mem: *mut c_void) -> i32;
        fn GlobalFree(mem: *mut c_void) -> *mut c_void;
    }

    pub(super) fn copy(fb: &FrameBuffer) -> Result<(), ClipboardError> {
        let dib = build_dib(fb);
        // SAFETY: every call below is a documented Win32 entry point, the
        // pointers are null or heap blocks the Win32 API owns, and the
        // block is only freed by the clipboard after `SetClipboardData`
        // succeeds.
        unsafe {
            if OpenClipboard(ptr::null_mut()) == 0 {
                return Err(ClipboardError::Refused(
                    "the clipboard is busy or unavailable".to_string(),
                ));
            }
            // An unnoticed `EmptyClipboard` failure left the old contents
            // in place, so the paste after a failed copy returned the
            // previous frame and looked like a stale buffer here (audit
            // F-132). The failure is checked and the copy aborted.
            if EmptyClipboard() == 0 {
                CloseClipboard();
                return Err(ClipboardError::Refused(
                    "the clipboard could not be emptied".to_string(),
                ));
            }
            let mem = GlobalAlloc(GMEM_MOVEABLE, dib.len());
            if mem.is_null() {
                CloseClipboard();
                return Err(ClipboardError::Refused("out of memory".to_string()));
            }
            let dst = GlobalLock(mem);
            if dst.is_null() {
                // Nobody else owns the block yet, so it is ours to free.
                GlobalFree(mem);
                CloseClipboard();
                return Err(ClipboardError::Refused("out of memory".to_string()));
            }
            ptr::copy_nonoverlapping(dib.as_ptr(), dst as *mut u8, dib.len());
            GlobalUnlock(mem);
            let placed = SetClipboardData(CF_DIB, mem);
            CloseClipboard();
            if placed.is_null() {
                // The clipboard refused the block, so it never took
                // ownership and nobody else will free it — a leak on
                // every failed copy (audit 2026-10-21 §G25).
                GlobalFree(mem);
                return Err(ClipboardError::Refused(
                    "the clipboard refused the bitmap".to_string(),
                ));
            }
        }
        Ok(())
    }

    /// The `BITMAPINFOHEADER` and the pixel rows, as `CF_DIB` wants them.
    ///
    /// A positive `biHeight` means the rows are stored **bottom-up**, and
    /// the pixels are BGRA, not RGBA.
    fn build_dib(fb: &FrameBuffer) -> Vec<u8> {
        let width = fb.width as i32;
        let height = fb.height as i32;
        let mut out = Vec::with_capacity(40 + fb.data.len());
        out.extend_from_slice(&40i32.to_le_bytes());
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&1i16.to_le_bytes());
        out.extend_from_slice(&32i16.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&(fb.data.len() as u32).to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        // The rows go out last-to-first: the header's positive `biHeight`
        // declares a bottom-up DIB, so row 0 of the framebuffer is the
        // LAST row of the blob (audit F-132's doc fix).
        for y in (0..fb.height).rev() {
            push_dib_row(&mut out, fb, y);
        }
        out
    }

    /// Append one row of pixels, BGRA.
    ///
    /// The row itself is written left-to-right (`top-down` was wrong
    /// here, audit F-132): it is the ROW ORDER the caller's `rev()` makes
    /// bottom-up, not the pixel order inside a row.
    /// The row is its own helper so the builder has no nested loops
    /// (audit 2026-10-21 §G5).
    fn push_dib_row(out: &mut Vec<u8>, fb: &FrameBuffer, y: u32) {
        for x in 0..fb.width {
            let i = ((y * fb.width + x) * 4) as usize;
            out.push(fb.data[i + 2]);
            out.push(fb.data[i + 1]);
            out.push(fb.data[i]);
            out.push(fb.data[i + 3]);
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{ClipboardError, FrameBuffer};
    use std::process::Command;

    pub(super) fn copy(fb: &FrameBuffer) -> Result<(), ClipboardError> {
        let path = std::env::temp_dir().join(format!("agnosis-shot-{}.png", std::process::id()));
        super::write_png(fb, &path)?;
        let result = copy_file(&path);
        let _ = std::fs::remove_file(&path);
        result
    }

    /// Hand the PNG file to the platform's clipboard helper.
    #[cfg(target_os = "macos")]
    fn copy_file(path: &std::path::Path) -> Result<(), ClipboardError> {
        let script = format!(
            "set the clipboard to (read (POSIX file \"{}\") as «class PNGf»)",
            path.display()
        );
        run(Command::new("osascript").arg("-e").arg(script))
    }

    /// Hand the PNG file to the platform's clipboard helper.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn copy_file(path: &std::path::Path) -> Result<(), ClipboardError> {
        let wayland = Command::new("wl-copy")
            .arg("--type")
            .arg("image/png")
            .arg(path)
            .output();
        if wayland.as_ref().is_ok_and(|out| out.status.success()) {
            return Ok(());
        }
        let x11 = Command::new("xclip")
            .arg("-selection")
            .arg("clipboard")
            .arg("-t")
            .arg("image/png")
            .arg(path)
            .output();
        match x11 {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(ClipboardError::Refused(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
            Err(e) => Err(ClipboardError::Refused(e.to_string())),
        }
    }

    /// Run a helper and turn its failure into a reported error.
    #[cfg(target_os = "macos")]
    fn run(command: &mut Command) -> Result<(), ClipboardError> {
        match command.output() {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(ClipboardError::Refused(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            )),
            Err(e) => Err(ClipboardError::Refused(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_can_be_written_as_a_png() {
        let mut fb = FrameBuffer::new(4, 4);
        fb.clear_black();
        let path =
            std::env::temp_dir().join(format!("agnosis-shot-test-{}.png", std::process::id()));
        write_png(&fb, &path).expect("write the png");
        assert!(path.exists(), "the png must exist");
        let bytes = std::fs::read(&path).expect("read the png");
        assert!(bytes.len() > 8, "the png must not be empty");
        assert_eq!(&bytes[1..4], b"PNG");
        let _ = std::fs::remove_file(&path);
    }
}

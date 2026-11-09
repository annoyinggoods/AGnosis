//! The package: a shareable file with the user's drawn art, the assignment
//! table and the config fragment (roadmap Extension 1 §3.3, §9.1).
//!
//! The archive is a **hand-rolled store-only ZIP** — no `zip` crate, no new
//! dependency, one static binary (roadmap §9.1 decision). The format is
//! simple enough to own:
//!
//! * a local file header per entry, a central directory, an
//!   end-of-central-directory record;
//! * method 0 (stored) — nothing is compressed;
//! * a CRC-32 (the IEEE polynomial) over the uncompressed data, **verified
//!   on every read**, so a corrupt archive is reported instead of being
//!   half-read.
//!
//! WHAT the import must never do (roadmap §9.1, decision: "a name clash is
//! refused with a message naming the existing asset, because a silent
//! overwrite is how a user's work is lost"):
//!
//! * an asset whose name is taken in the target folder is **refused** — it
//!   is skipped and named in the report, never overwritten;
//! * an assignment that points at an asset that was not imported is
//!   **kept** — the asset may appear later;
//! * the user's own assignments **win**: the table is merged per slot,
//!   never replaced, and every conflict is reported;
//! * the assignments are written with
//!   [`crate::config_edit::write_assignments`], which replaces **only**
//!   the `[assignments]` table — the user's comments and unknown keys
//!   survive.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{AssignmentSlot, Config, ConfigError};
use crate::drawn::{DrawnDoc, DrawnKind, DrawnSprite};
use crate::manifest::{
    is_safe_relative_path, Manifest, ManifestAsset, ManifestError, MANIFEST_ENTRY,
};

// ---------------------------------------------------------------------------
// CRC-32 (IEEE)
// ---------------------------------------------------------------------------

/// The CRC-32 table, built once at compile time.
const CRC_TABLE: [u32; 256] = build_crc_table();

/// Build the CRC-32 table: the reflected IEEE polynomial.
///
/// The eight bit-steps are **unrolled** into eight calls: a loop inside a
/// loop is banned (audit nesting rule), and a table built at compile time
/// costs nothing at runtime.
const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = index as u32;
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        crc = crc_step(crc);
        table[index] = crc;
        index += 1;
    }
    table
}

/// One step of the reflected CRC-32: shift, and divide by the polynomial
/// when the lowest bit is set.
const fn crc_step(crc: u32) -> u32 {
    if crc & 1 == 1 {
        0xEDB8_8320 ^ (crc >> 1)
    } else {
        crc >> 1
    }
}

/// The CRC-32 of `data` (the IEEE polynomial, the one the ZIP format
/// specifies).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in data {
        let index = ((crc ^ *byte as u32) & 0xFF) as usize;
        crc = CRC_TABLE[index] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

// ---------------------------------------------------------------------------
// The store-only ZIP
// ---------------------------------------------------------------------------

/// The signature of a local file header.
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
/// The signature of a central directory record.
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
/// The signature of the end-of-central-directory record.
const EOCD_SIGNATURE: u32 = 0x0605_4b50;

/// The length of a local file header without its name.
const LOCAL_HEADER_LEN: usize = 30;
/// The length of a central directory record without its name.
const CENTRAL_HEADER_LEN: usize = 46;
/// The length of the end-of-central-directory record.
const EOCD_LEN: usize = 22;
/// The longest comment a ZIP may carry, so the end record can be found.
const MAX_COMMENT_LEN: usize = 0xFFFF;

/// One entry of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipEntry {
    /// The name of the entry, always with `/` separators.
    pub name: String,
    /// The CRC-32 of the stored data.
    pub crc32: u32,
    /// The size of the stored data in bytes.
    pub size: u32,
    /// The offset of the entry's local header in the archive.
    pub offset: u32,
}

/// A ZIP writer that stores its entries uncompressed.
#[derive(Debug, Default)]
pub struct ZipWriter {
    body: Vec<u8>,
    central: Vec<u8>,
    count: usize,
}

impl ZipWriter {
    /// A new, empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one stored file.
    ///
    /// The name must stay **inside** the archive: an absolute path, a
    /// `..` component or a backslash is refused.
    pub fn add_file(&mut self, name: &str, data: &[u8]) -> Result<(), ZipError> {
        if !is_safe_relative_path(name) {
            return Err(ZipError::UnsafeName {
                name: name.to_string(),
            });
        }
        let name_bytes = name.as_bytes();
        let name_len = u16::try_from(name_bytes.len()).map_err(|_| ZipError::TooLarge)?;
        let size = u32::try_from(data.len()).map_err(|_| ZipError::TooLarge)?;
        let offset = u32::try_from(self.body.len()).map_err(|_| ZipError::TooLarge)?;
        let crc = crc32(data);
        self.body
            .extend_from_slice(&local_header(name_bytes, name_len, crc, size));
        self.body.extend_from_slice(data);
        self.central
            .extend_from_slice(&central_header(name_bytes, name_len, crc, size, offset));
        self.count += 1;
        Ok(())
    }

    /// The finished archive.
    pub fn finish(self) -> Result<Vec<u8>, ZipError> {
        let ZipWriter {
            body,
            central,
            count,
        } = self;
        let count = u16::try_from(count).map_err(|_| ZipError::TooLarge)?;
        let cd_size = u32::try_from(central.len()).map_err(|_| ZipError::TooLarge)?;
        let cd_offset = u32::try_from(body.len()).map_err(|_| ZipError::TooLarge)?;
        let mut out = body;
        out.extend_from_slice(&central);
        out.extend_from_slice(&end_of_central_directory(count, cd_size, cd_offset));
        Ok(out)
    }
}

/// The local file header of one entry.
fn local_header(name: &[u8], name_len: u16, crc: u32, size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(LOCAL_HEADER_LEN + name.len());
    out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes()); // compressed size
    out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
    out.extend_from_slice(name);
    out
}

/// The central directory record of one entry.
fn central_header(name: &[u8], name_len: u16, crc: u32, size: u32, offset: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(CENTRAL_HEADER_LEN + name.len());
    out.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&20u16.to_le_bytes()); // version made by
    out.extend_from_slice(&20u16.to_le_bytes()); // version needed
    out.extend_from_slice(&0u16.to_le_bytes()); // flags
    out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
    out.extend_from_slice(&0u16.to_le_bytes()); // mod time
    out.extend_from_slice(&0u16.to_le_bytes()); // mod date
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes()); // compressed size
    out.extend_from_slice(&size.to_le_bytes()); // uncompressed size
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // extra field length
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
    out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
    out.extend_from_slice(&0u32.to_le_bytes()); // external attributes
    out.extend_from_slice(&offset.to_le_bytes());
    out.extend_from_slice(name);
    out
}

/// The end-of-central-directory record.
fn end_of_central_directory(count: u16, cd_size: u32, cd_offset: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(EOCD_LEN);
    out.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // this disk
    out.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
    out.extend_from_slice(&count.to_le_bytes()); // entries on this disk
    out.extend_from_slice(&count.to_le_bytes()); // entries in total
    out.extend_from_slice(&cd_size.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // comment length
    out
}

/// A parsed ZIP archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZipArchive {
    bytes: Vec<u8>,
    entries: Vec<ZipEntry>,
}

impl ZipArchive {
    /// Parse an archive.
    ///
    /// The end-of-central-directory record is found from the back of the
    /// file, the central directory is walked, and every entry's name is
    /// checked: an entry that leaves the archive root is refused before
    /// any of its data is read.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, ZipError> {
        let eocd = find_end_of_central_directory(&bytes)?;
        let count = read_u16(&bytes, eocd.saturating_add(10))?;
        let cd_size = read_u32(&bytes, eocd.saturating_add(12))?;
        let cd_offset = read_u32(&bytes, eocd.saturating_add(16))?;
        let entries = read_central_directory(&bytes, cd_offset, cd_size, count)?;
        Ok(Self { bytes, entries })
    }

    /// The entries of the archive, in the order the directory lists them.
    pub fn entries(&self) -> &[ZipEntry] {
        &self.entries
    }

    /// Does the archive carry an entry of that name?
    pub fn has(&self, name: &str) -> bool {
        self.entries.iter().any(|entry| entry.name == name)
    }

    /// The data of one entry.
    ///
    /// The CRC-32 is **verified**: a mismatch is an error, never a
    /// half-read file.
    pub fn read(&self, name: &str) -> Result<&[u8], ZipError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| ZipError::NotFound {
                name: name.to_string(),
            })?;
        let data = self.entry_data(entry)?;
        let actual = crc32(data);
        if actual != entry.crc32 {
            return Err(ZipError::CrcMismatch {
                name: entry.name.clone(),
                expected: entry.crc32,
                actual,
            });
        }
        Ok(data)
    }

    /// The stored bytes of one entry, without the CRC check.
    fn entry_data(&self, entry: &ZipEntry) -> Result<&[u8], ZipError> {
        let offset = entry.offset as usize;
        let header = self.bytes.get(offset..).ok_or(ZipError::Truncated)?;
        if read_u32(header, 0)? != LOCAL_SIGNATURE {
            return Err(ZipError::BadSignature);
        }
        let method = read_u16(header, 8)?;
        if method != 0 {
            return Err(ZipError::UnsupportedMethod {
                name: entry.name.clone(),
                method,
            });
        }
        let name_len = read_u16(header, 26)? as usize;
        let extra_len = read_u16(header, 28)? as usize;
        let start = offset
            .saturating_add(LOCAL_HEADER_LEN)
            .saturating_add(name_len)
            .saturating_add(extra_len);
        let end = start.saturating_add(entry.size as usize);
        self.bytes.get(start..end).ok_or(ZipError::Truncated)
    }
}

/// Find the end-of-central-directory record, searching from the back.
fn find_end_of_central_directory(bytes: &[u8]) -> Result<usize, ZipError> {
    if bytes.len() < EOCD_LEN {
        return Err(ZipError::Truncated);
    }
    let window_len = bytes.len().min(EOCD_LEN + MAX_COMMENT_LEN);
    let start = bytes.len() - window_len;
    let window = &bytes[start..];
    let last = window.len() - EOCD_LEN;
    for offset in (0..=last).rev() {
        if read_u32(window, offset)? == EOCD_SIGNATURE {
            return Ok(start + offset);
        }
    }
    Err(ZipError::BadSignature)
}

/// Walk the central directory and list its entries.
fn read_central_directory(
    bytes: &[u8],
    offset: u32,
    size: u32,
    count: u16,
) -> Result<Vec<ZipEntry>, ZipError> {
    let start = offset as usize;
    let end = start.saturating_add(size as usize);
    let directory = bytes.get(start..end).ok_or(ZipError::Truncated)?;
    let mut entries = Vec::with_capacity(count as usize);
    let mut cursor = 0usize;
    for _ in 0..count {
        if read_u32(directory, cursor)? != CENTRAL_SIGNATURE {
            return Err(ZipError::BadSignature);
        }
        let method = read_u16(directory, cursor.saturating_add(10))?;
        let crc = read_u32(directory, cursor.saturating_add(16))?;
        let size = read_u32(directory, cursor.saturating_add(24))?;
        let name_len = read_u16(directory, cursor.saturating_add(28))? as usize;
        let extra_len = read_u16(directory, cursor.saturating_add(30))? as usize;
        let comment_len = read_u16(directory, cursor.saturating_add(32))? as usize;
        let local_offset = read_u32(directory, cursor.saturating_add(42))?;
        let name_start = cursor.saturating_add(CENTRAL_HEADER_LEN);
        let name_bytes = directory
            .get(name_start..name_start.saturating_add(name_len))
            .ok_or(ZipError::Truncated)?;
        let name = String::from_utf8(name_bytes.to_vec()).map_err(|_| ZipError::NotUtf8)?;
        if !is_safe_relative_path(&name) {
            return Err(ZipError::UnsafeName { name });
        }
        if method != 0 {
            return Err(ZipError::UnsupportedMethod { name, method });
        }
        entries.push(ZipEntry {
            name,
            crc32: crc,
            size,
            offset: local_offset,
        });
        cursor = cursor
            .saturating_add(CENTRAL_HEADER_LEN)
            .saturating_add(name_len)
            .saturating_add(extra_len)
            .saturating_add(comment_len);
    }
    Ok(entries)
}

/// Read a little-endian `u16` at `at`.
fn read_u16(bytes: &[u8], at: usize) -> Result<u16, ZipError> {
    let end = at.saturating_add(2);
    let slice = bytes.get(at..end).ok_or(ZipError::Truncated)?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

/// Read a little-endian `u32` at `at`.
fn read_u32(bytes: &[u8], at: usize) -> Result<u32, ZipError> {
    let end = at.saturating_add(4);
    let slice = bytes.get(at..end).ok_or(ZipError::Truncated)?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Everything that can go wrong while reading or writing a ZIP archive.
#[derive(Debug, thiserror::Error)]
pub enum ZipError {
    /// The file is not a ZIP archive.
    #[error("the file is not a zip archive (bad signature)")]
    BadSignature,
    /// The archive ends in the middle of something.
    #[error("the zip archive is truncated")]
    Truncated,
    /// An entry's name would leave the archive root.
    #[error("unsafe file name {name:?} in the archive")]
    UnsafeName { name: String },
    /// An entry's name is not valid UTF-8.
    #[error("the archive carries an entry whose name is not valid utf-8")]
    NotUtf8,
    /// The archive has no entry of that name.
    #[error("the archive has no entry named {name:?}")]
    NotFound { name: String },
    /// The stored data does not match its CRC-32.
    #[error(
        "crc mismatch in {name:?}: the archive says {expected:#010x}, the data is {actual:#010x}"
    )]
    CrcMismatch {
        name: String,
        expected: u32,
        actual: u32,
    },
    /// The entry is compressed; this build reads stored entries only.
    #[error("unsupported compression method {method} in {name:?} (this build reads stored entries only)")]
    UnsupportedMethod { name: String, method: u16 },
    /// The archive does not fit the 32-bit ZIP format.
    #[error("the archive is too large for the 32-bit zip format")]
    TooLarge,
}

// ---------------------------------------------------------------------------
// The package
// ---------------------------------------------------------------------------

/// The bytes of one drawn asset: the drawn document's JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetPayload {
    /// What kind of asset it is.
    pub kind: DrawnKind,
    /// The asset's name — it becomes a file name on import.
    pub name: String,
    /// The document's bytes.
    pub bytes: Vec<u8>,
}

/// Where an import writes to.
///
/// The folders are a parameter so the tests point them at a temp dir and
/// never touch the user's real assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetDirs {
    /// The user's `config.toml`.
    pub config_file: PathBuf,
    /// The folder of the drawn sprites.
    pub sprites: PathBuf,
    /// The folder of the drawn balloons.
    pub balloons: PathBuf,
    /// The folder of the drawn grass tiles.
    pub grass: PathBuf,
    /// The folder of the drawn duck decorations.
    pub decorations: PathBuf,
}

impl TargetDirs {
    /// The folder that holds the assets of `kind`.
    pub fn dir_for(&self, kind: DrawnKind) -> &Path {
        match kind {
            DrawnKind::Sprite => &self.sprites,
            DrawnKind::Balloon => &self.balloons,
            DrawnKind::Grass => &self.grass,
            DrawnKind::Decoration => &self.decorations,
        }
    }
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// The assets that were written.
    pub imported: Vec<String>,
    /// The assets that were **not** written, with the reason.
    pub skipped: Vec<String>,
    /// How many assignments were merged into the user's table.
    pub merged_assignments: usize,
    /// The assignments the user already had; the user's own work won.
    pub conflicts: Vec<String>,
}

impl ImportReport {
    /// A human-readable summary of the import.
    ///
    /// Test-only: the core does not build user-facing strings — the CLI
    /// renders the report from the `pub` fields at the edge (audit
    /// 2026-10-23 §F-063).
    #[cfg(test)]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("IMPORTED {}\n", self.imported.len()));
        push_lines(&mut out, &self.imported);
        out.push_str(&format!("SKIPPED {}\n", self.skipped.len()));
        push_lines(&mut out, &self.skipped);
        out.push_str(&format!("ASSIGNMENTS {}\n", self.merged_assignments));
        out.push_str(&format!("CONFLICTS {}\n", self.conflicts.len()));
        push_lines(&mut out, &self.conflicts);
        out
    }
}

/// Append a list of lines, each indented.
#[cfg(test)]
fn push_lines(out: &mut String, lines: &[String]) {
    for line in lines {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// The look (audit 2026-10-23 §F-045 / §F-046)
// ---------------------------------------------------------------------------

/// The **look** of a configuration: everything a shared file may carry.
///
/// One capture policy for every shared artifact (a package, a preset):
/// the sky, the weather, the message, the grass, the assignments, the
/// personalities, the backdrops and the decorations. The hotkey chords
/// and the browser list are the **user's machine's** settings — they stay
/// at the defaults here and are removed from the fragment entirely by
/// [`Manifest::from_look`], so a `.agnosis` file that is sent to somebody
/// else leaks nothing and resets nothing (audit 2026-10-23 §F-046; the
/// two policies used to differ between `export_all` and `save_preset`).
///
/// A backdrop is a **name of a picture**, and the picture is not in the
/// package: applying the name on a machine without the picture is a
/// skipped layer, never an error (the loader's own rule).
pub fn capture_look(cfg: &Config) -> Config {
    Config {
        sky: cfg.sky.clone(),
        weather: cfg.weather.clone(),
        message: cfg.message.clone(),
        grass: cfg.grass.clone(),
        assignments: cfg.assignments.clone(),
        personality: cfg.personality.clone(),
        backdrop: cfg.backdrop.clone(),
        decorations: cfg.decorations.clone(),
        ..Config::default()
    }
}

/// Apply a look fragment to the user's config **file**, surgically and in
/// one write.
///
/// This is the import half of the README's promise that `agnosis import`
/// applies the look (audit 2026-10-23 §F-045): the package's assignments
/// are merged by [`import_from`]; this applies the fragment's
/// `[sky]`/`[weather]`/`[message]`/`[grass]` tables in place, so the
/// user's comments, unknown keys, hotkey and browser list survive. All
/// four splices are built in memory and land in ONE atomic write — an
/// import is one transaction, not four (audit 2026-10-23 §F-051's
/// discipline, shared with `preset::apply_preset_to_file`).
///
/// A file that does not exist yet is created from the whole default
/// config, the same fresh path the single-table writers have.
pub fn apply_look_from_fragment(path: &Path, fragment: &Config) -> Result<(), ConfigError> {
    let original = crate::config_edit::read_or_default(path)?;
    let spliced = splice_look(&original, fragment)
        .and_then(crate::config_edit::checked_config_text)
        .ok_or_else(|| ConfigError::TableWrite {
            section: "look".to_string(),
            path: path.to_path_buf(),
        })?;
    crate::config_edit::write_atomic(path, &spliced)
}

/// Splice the look's four tables into a document, in memory.
///
/// `pub(crate)`: `preset::apply_preset_to_file` composes these tables
/// with its assignments and personality splices into the same single
/// write (audit 2026-10-23 §F-051).
///
/// `None` when the document cannot carry one of the tables (an inline
/// `grass = { … }` key, say): the caller then refuses the whole edit.
pub(crate) fn splice_look(text: &str, fragment: &Config) -> Option<String> {
    let out = crate::config_edit::splice_table(text, "sky", &sky_entries(fragment))?;
    let out = crate::config_edit::splice_table(&out, "weather", &weather_entries(fragment))?;
    let out = crate::config_edit::splice_table(&out, "message", &message_entries(fragment))?;
    crate::config_edit::splice_table(&out, "grass", &grass_entries(fragment))
}

/// The `[sky]` table's entries.
pub(crate) fn sky_entries(cfg: &Config) -> Vec<(&'static str, Option<String>)> {
    vec![
        (
            "enabled",
            Some(toml::Value::Boolean(cfg.sky.enabled).to_string()),
        ),
        (
            "time_of_day",
            Some(toml::Value::Float(cfg.sky.time_of_day as f64).to_string()),
        ),
    ]
}

/// The `[weather]` table's entries.
pub(crate) fn weather_entries(cfg: &Config) -> Vec<(&'static str, Option<String>)> {
    vec![
        (
            "mode",
            Some(toml::Value::String(cfg.weather.mode.as_str().to_string()).to_string()),
        ),
        (
            "intensity",
            Some(toml::Value::Float(cfg.weather.intensity as f64).to_string()),
        ),
        (
            "seed",
            Some(toml::Value::Integer(cfg.weather.seed as i64).to_string()),
        ),
    ]
}

/// The `[message]` table's entries.
pub(crate) fn message_entries(cfg: &Config) -> Vec<(&'static str, Option<String>)> {
    vec![
        (
            "text",
            Some(toml::Value::String(cfg.message.text.clone()).to_string()),
        ),
        (
            "position",
            Some(
                toml::Value::String(message_position_name(cfg.message.position).to_string())
                    .to_string(),
            ),
        ),
        (
            "fade_seconds",
            Some(toml::Value::Float(cfg.message.fade_seconds as f64).to_string()),
        ),
    ]
}

/// The `[grass]` table's entries.
///
/// `None` for the tile **clears** the key: a look that carries no grass
/// tile must be able to remove the user's.
pub(crate) fn grass_entries(cfg: &Config) -> Vec<(&'static str, Option<String>)> {
    let tile = cfg
        .grass
        .tile
        .as_ref()
        .map(|t| toml::Value::String(t.clone()).to_string());
    vec![("tile", tile)]
}

/// The name a message position is stored under.
fn message_position_name(position: crate::config::MessagePosition) -> &'static str {
    match position {
        crate::config::MessagePosition::AboveGrass => "above_grass",
        crate::config::MessagePosition::BottomLeft => "bottom_left",
        crate::config::MessagePosition::BottomRight => "bottom_right",
    }
}

/// Write a package: the manifest plus the asset documents.
pub fn export_to(
    path: &Path,
    manifest: &Manifest,
    assets: &[AssetPayload],
) -> Result<(), PackageError> {
    manifest.validate()?;
    check_payloads_match(manifest, assets)?;
    let mut writer = ZipWriter::new();
    writer.add_file(MANIFEST_ENTRY, manifest.to_json()?.as_bytes())?;
    for payload in assets {
        let asset = manifest.asset(payload.kind, &payload.name).ok_or_else(|| {
            PackageError::UndescribedAsset {
                name: payload.name.clone(),
            }
        })?;
        writer.add_file(&asset.file, &payload.bytes)?;
    }
    let bytes = writer.finish()?;
    write_bytes(path, &bytes)?;
    Ok(())
}

/// Write just the manifest as JSON — what `agnosis config export` produces.
pub fn export_manifest(path: &Path, manifest: &Manifest) -> Result<(), PackageError> {
    manifest.validate()?;
    let text = manifest.to_json()?;
    write_bytes(path, text.as_bytes())?;
    Ok(())
}

/// Export everything the user has: the drawn assets, the assignment table
/// and the config fragment.
///
/// This is what `agnosis export out.agnosis` calls: one call, one file.
///
/// The config side is the **look** ([`capture_look`]): the fragment a
/// shared package carries must not contain the user's hotkey chords or
/// browser list (audit 2026-10-23 §F-046).
///
/// The target folders are a **parameter**: the core never resolves a
/// platform location, so the shell hands them over (the tests point them
/// at a temp dir, so a test never reads the user's real assets).
pub fn export_all(
    path: &Path,
    cfg: &Config,
    name: &str,
    target: &TargetDirs,
) -> Result<(), PackageError> {
    let payloads = all_payloads(target)?;
    let mut manifest = Manifest::from_look(&capture_look(cfg), name);
    for payload in &payloads {
        manifest.add_asset(payload.kind, &payload.name);
    }
    export_to(path, &manifest, &payloads)
}

/// The manifest and the payloads must describe the same assets.
fn check_payloads_match(manifest: &Manifest, assets: &[AssetPayload]) -> Result<(), PackageError> {
    for payload in assets {
        if manifest.asset(payload.kind, &payload.name).is_none() {
            return Err(PackageError::UndescribedAsset {
                name: payload.name.clone(),
            });
        }
    }
    for asset in &manifest.assets {
        let described = assets
            .iter()
            .any(|p| p.kind == asset.kind && p.name == asset.name);
        if !described {
            return Err(PackageError::MissingDocument {
                name: asset.name.clone(),
            });
        }
    }
    Ok(())
}

/// Read a package and apply it to the target folders.
pub fn import_from(path: &Path, target: &TargetDirs) -> Result<ImportReport, PackageError> {
    let bytes = std::fs::read(path).map_err(|source| PackageError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let archive = ZipArchive::parse(bytes)?;
    let manifest_text = archive.read(MANIFEST_ENTRY)?;
    let text = std::str::from_utf8(manifest_text).map_err(|_| PackageError::NotUtf8)?;
    let manifest = Manifest::from_json(text)?;
    manifest.validate()?;
    let mut report = ImportReport::default();
    import_assets(&archive, &manifest, target, &mut report)?;
    merge_assignments(&manifest, target, &mut report)?;
    Ok(report)
}

/// Write the package's assets into the target folders.
///
/// The assets are **decoded first** and written second: a package that
/// carries a broken document is refused before anything is written, so an
/// import never leaves half a package behind.
///
/// An asset whose name is already taken is **refused**: it is skipped and
/// named in the report, and the user's file is never overwritten.
fn import_assets(
    archive: &ZipArchive,
    manifest: &Manifest,
    target: &TargetDirs,
    report: &mut ImportReport,
) -> Result<(), PackageError> {
    let decoded = decode_assets(archive, manifest, report)?;
    for (asset, sprite) in decoded {
        let dir = target.dir_for(asset.kind);
        if crate::drawn::list_in(dir)
            .iter()
            .any(|name| name == &asset.name)
        {
            report.skipped.push(format!(
                "{}: a {} of that name already exists, refused",
                asset.name, asset.kind
            ));
            continue;
        }
        crate::drawn::save_to(dir, &sprite, asset.kind, &asset.name)?;
        report.imported.push(asset.name.clone());
    }
    Ok(())
}

/// Decode every asset of the package, before anything is written.
fn decode_assets<'a>(
    archive: &ZipArchive,
    manifest: &'a Manifest,
    report: &mut ImportReport,
) -> Result<Vec<(&'a ManifestAsset, DrawnSprite)>, PackageError> {
    let mut decoded = Vec::new();
    for asset in &manifest.assets {
        let Some(sprite) = decode_asset(archive, asset)? else {
            report.skipped.push(format!(
                "{}: the package carries no document for it",
                asset.name
            ));
            continue;
        };
        decoded.push((asset, sprite));
    }
    Ok(decoded)
}

/// Decode one asset of the package into a runtime asset.
///
/// `None` when the package carries no document for it: the archive and the
/// manifest disagree, and the asset is skipped rather than faked.
fn decode_asset(
    archive: &ZipArchive,
    asset: &ManifestAsset,
) -> Result<Option<DrawnSprite>, PackageError> {
    if !archive.has(&asset.file) {
        return Ok(None);
    }
    let bytes = archive.read(&asset.file)?;
    let doc: DrawnDoc = serde_json::from_slice(bytes).map_err(|source| PackageError::Json {
        name: asset.name.clone(),
        source,
    })?;
    if doc.kind != asset.kind {
        return Err(PackageError::KindMismatch {
            name: asset.name.clone(),
            expected: asset.kind,
            actual: doc.kind,
        });
    }
    let sprite = DrawnSprite::from_doc(&doc)?;
    Ok(Some(sprite))
}

/// Merge the package's assignment table into the user's own table.
///
/// The rules, one by one:
///
/// * the user's own assignment **wins** — the imported one is reported as
///   a conflict and dropped;
/// * a slot the user has not assigned is filled;
/// * an assignment that points at an asset that was not imported is
///   **kept**: the asset may appear later;
/// * the table is written **surgically**: only the `[assignments]` table
///   is replaced, so the user's comments and unknown keys survive.
fn merge_assignments(
    manifest: &Manifest,
    target: &TargetDirs,
    report: &mut ImportReport,
) -> Result<(), PackageError> {
    if manifest.assignments.is_empty() {
        return Ok(());
    }
    let cfg = load_user_config(&target.config_file)?;
    let mut merged = cfg.assignments.clone();
    let mut current = current_assignments(&cfg);
    for (slot, name) in &manifest.assignments {
        let parsed = AssignmentSlot::parse(slot)
            .map_err(|_| ManifestError::UnknownSlot { slot: slot.clone() })?;
        let key = parsed.key();
        if let Some(kept) = current.get(&key) {
            report.conflicts.push(format!(
                "{key}: keeps \"{kept}\" (the package wanted \"{name}\")"
            ));
            continue;
        }
        merged.set(parsed, Some(name.clone()));
        current.insert(key, name.clone());
        report.merged_assignments += 1;
    }
    crate::config_edit::write_assignments(&target.config_file, &merged)?;
    Ok(())
}

/// The user's own assignment table, as a slot -> name map.
fn current_assignments(cfg: &Config) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (slot, value) in cfg.assignments.entries() {
        let Some(value) = value else {
            continue;
        };
        map.insert(slot, value.to_string());
    }
    map
}

/// The user's config, **unsanitized**: the repairing loader would drop the
/// assignments whose asset does not exist (yet), and that is a deletion
/// the user never asked for.
fn load_user_config(path: &Path) -> Result<Config, PackageError> {
    if !path.exists() {
        return Ok(Config::default());
    }
    Ok(Config::load_unsanitized_from(path)?)
}

/// The payload of a drawn asset in `dir`.
pub fn payload_in(dir: &Path, kind: DrawnKind, name: &str) -> Result<AssetPayload, PackageError> {
    let path = dir.join(format!("{name}.json"));
    let bytes = std::fs::read(&path).map_err(|source| PackageError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(AssetPayload {
        kind,
        name: name.to_string(),
        bytes,
    })
}

/// The payloads of every drawn asset in the target folders.
pub fn all_payloads(target: &TargetDirs) -> Result<Vec<AssetPayload>, PackageError> {
    let mut payloads = Vec::new();
    for kind in [
        DrawnKind::Sprite,
        DrawnKind::Balloon,
        DrawnKind::Grass,
        DrawnKind::Decoration,
    ] {
        payloads.extend(payloads_in(target, kind)?);
    }
    Ok(payloads)
}

/// The payloads of one kind, in one folder.
///
/// A helper of its own: the folders are walked one kind at a time, so
/// there is no loop inside a loop (audit nesting rule).
fn payloads_in(target: &TargetDirs, kind: DrawnKind) -> Result<Vec<AssetPayload>, PackageError> {
    let dir = target.dir_for(kind);
    let mut payloads = Vec::new();
    for name in crate::drawn::list_in(dir) {
        payloads.push(payload_in(dir, kind, &name)?);
    }
    Ok(payloads)
}

/// Write a file atomically, creating the parent folder.
///
/// WHY atomic: `std::fs::write` truncates the target first, so a crash
/// (or the release profile's `panic = "abort"`) mid-write left a
/// truncated `.agnosis` package behind — and a truncated package is not a
/// package at all (audit 2026-10-21 §F11).
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    ensure_parent(path)?;
    let temp = temp_path(path);
    std::fs::write(&temp, bytes).map_err(|source| PackageError::Io {
        path: temp.clone(),
        source,
    })?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(PackageError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// The temp file a write goes through: a sibling of the target, named
/// after it and the writing process, so two writers do not collide.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "package.agnosis".to_string());
    match path.parent() {
        Some(parent) => parent.join(format!("{name}.{}.tmp", std::process::id())),
        None => PathBuf::from(format!("{name}.{}.tmp", std::process::id())),
    }
}

/// Create the parent folder of `path`, when it has one.
fn ensure_parent(path: &Path) -> Result<(), PackageError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|source| PackageError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Everything that can go wrong while exporting or importing a package.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// A file could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The archive is broken.
    #[error(transparent)]
    Zip(#[from] ZipError),
    /// The manifest is broken.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A drawn document is broken.
    #[error(transparent)]
    Drawn(#[from] crate::drawn::DrawnError),
    /// The config could not be read or written.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
    /// The platform folders could not be resolved.
    #[error("could not resolve the platform folders: {source}")]
    Path {
        #[source]
        source: crate::path_error::PathError,
    },
    /// The package carries an asset the manifest does not describe.
    #[error("the asset {name:?} is not described by the manifest")]
    UndescribedAsset { name: String },
    /// The manifest describes an asset the package does not carry.
    #[error(
        "the manifest describes the asset {name:?}, but the package carries no document for it"
    )]
    MissingDocument { name: String },
    /// A drawn document is not valid JSON.
    #[error("the document of {name:?} is not valid json: {source}")]
    Json {
        name: String,
        #[source]
        source: serde_json::Error,
    },
    /// The document's kind is not the kind the manifest says.
    #[error("the asset {name:?} is a {actual}, but the manifest says {expected}")]
    KindMismatch {
        name: String,
        expected: DrawnKind,
        actual: DrawnKind,
    },
    /// An entry's name is not valid UTF-8.
    #[error("the package carries an entry whose name is not valid utf-8")]
    NotUtf8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balloon::AsciiSprite;
    use crate::drawn::{DrawnSize, PALETTE};

    /// A temp folder that is not the user's real config folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ducks-package-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// A small sprite with one inked cell.
    fn small_sprite() -> DrawnSprite {
        let (cols, rows) = crate::drawn::sprite_frame_size(DrawnSize::Small);
        let mut frame = AsciiSprite::new(cols, rows);
        frame.set(0, 0, 'x', PALETTE[0]);
        DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Small),
            frames: vec![frame],
            reaction: None,
        }
    }

    /// The bytes of a drawn document.
    fn sprite_bytes(sprite: &DrawnSprite) -> Vec<u8> {
        serde_json::to_vec(&sprite.to_doc()).expect("serialize the document")
    }

    /// A target that points at temp folders.
    fn target_dirs(root: &Path) -> TargetDirs {
        let sprites = root.join("sprites");
        let balloons = root.join("balloons");
        let grass = root.join("grass");
        let decorations = root.join("decorations");
        for dir in [&sprites, &balloons, &grass, &decorations] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        TargetDirs {
            config_file: root.join("config.toml"),
            sprites,
            balloons,
            grass,
            decorations,
        }
    }

    #[test]
    fn the_crc_matches_the_known_vector() {
        // The CRC-32 of "123456789" is 0xCBF43926 — the value every ZIP
        // implementation agrees on.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn the_zip_round_trip_compares_the_bytes_exactly() {
        let mut writer = ZipWriter::new();
        writer.add_file("a.txt", b"hello").expect("add");
        writer
            .add_file("dir/b.bin", &[0u8, 1, 2, 255])
            .expect("add");
        writer.add_file("c.txt", b"").expect("add");
        let bytes = writer.finish().expect("finish");

        let archive = ZipArchive::parse(bytes).expect("parse");
        assert_eq!(archive.entries().len(), 3);
        assert_eq!(archive.read("a.txt").expect("read"), b"hello");
        assert_eq!(archive.read("dir/b.bin").expect("read"), &[0u8, 1, 2, 255]);
        assert_eq!(archive.read("c.txt").expect("read"), b"");
        assert!(archive.has("a.txt"));
        assert!(!archive.has("nope.txt"));
    }

    #[test]
    fn a_flipped_byte_is_caught_by_the_crc() {
        let mut writer = ZipWriter::new();
        writer.add_file("a.txt", b"hello world").expect("add");
        let mut bytes = writer.finish().expect("finish");
        // Flip a byte inside the stored data (after the local header and
        // the name).
        let at = 30 + "a.txt".len() + 2;
        bytes[at] ^= 0xFF;
        let archive = ZipArchive::parse(bytes).expect("parse");
        let err = archive.read("a.txt").expect_err("the crc must fail");
        assert!(matches!(err, ZipError::CrcMismatch { .. }), "{err:?}");
    }

    #[test]
    fn a_bad_signature_is_refused() {
        let err = ZipArchive::parse(b"not a zip at all, really not".to_vec())
            .expect_err("a bad signature is refused");
        assert!(
            matches!(err, ZipError::BadSignature | ZipError::Truncated),
            "{err:?}"
        );
    }

    #[test]
    fn a_truncated_archive_is_refused() {
        let mut writer = ZipWriter::new();
        writer.add_file("a.txt", b"hello").expect("add");
        let bytes = writer.finish().expect("finish");
        let cut = &bytes[..bytes.len() - 4];
        let err = ZipArchive::parse(cut.to_vec()).expect_err("a truncated archive is refused");
        assert!(
            matches!(err, ZipError::Truncated | ZipError::BadSignature),
            "{err:?}"
        );
    }

    #[test]
    fn an_unsafe_entry_name_is_refused_on_write_and_on_read() {
        let mut writer = ZipWriter::new();
        let err = writer
            .add_file("../escape.txt", b"x")
            .expect_err("an unsafe name is refused");
        assert!(matches!(err, ZipError::UnsafeName { .. }), "{err:?}");

        // A hand-built archive with an escaping name is refused on read.
        // The name appears twice — in the local header and in the central
        // directory — and both are rewritten, because the central
        // directory is what the reader trusts.
        let mut raw = ZipWriter::new();
        raw.add_file("a.txt", b"x").expect("add");
        let mut bytes = raw.finish().expect("finish");
        let name = b"a.txt";
        let mut at = 0usize;
        while let Some(found) = bytes[at..].windows(name.len()).position(|w| w == name) {
            let start = at + found;
            bytes[start..start + name.len()].copy_from_slice(b"../x.");
            at = start + name.len();
        }
        let err = ZipArchive::parse(bytes).expect_err("an unsafe name is refused");
        assert!(matches!(err, ZipError::UnsafeName { .. }), "{err:?}");
    }

    #[test]
    fn a_package_round_trips_through_the_filesystem() {
        let root = temp_dir("round-trip");
        let target = target_dirs(&root);
        let sprite = small_sprite();
        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        manifest
            .assignments
            .insert("small_1".to_string(), "sprite-1".to_string());
        let payload = AssetPayload {
            kind: DrawnKind::Sprite,
            name: "sprite-1".to_string(),
            bytes: sprite_bytes(&sprite),
        };
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[payload]).expect("export");

        let report = import_from(&path, &target).expect("import");
        assert_eq!(report.imported, vec!["sprite-1".to_string()]);
        assert_eq!(report.merged_assignments, 1);
        assert!(report.conflicts.is_empty());

        // The asset is loadable and the assignment landed.
        let loaded = crate::drawn::load_from(&target.sprites, DrawnKind::Sprite, "sprite-1")
            .expect("the asset must load");
        assert_eq!(loaded.to_doc(), sprite.to_doc());
        let cfg = Config::load_unsanitized_from(&target.config_file).expect("the config");
        assert_eq!(cfg.assignments.small(1), Some("sprite-1"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_existing_asset_is_refused_and_named() {
        let root = temp_dir("clash");
        let target = target_dirs(&root);
        let sprite = small_sprite();
        // The user already has an asset of that name.
        crate::drawn::save_to(&target.sprites, &sprite, DrawnKind::Sprite, "sprite-1")
            .expect("the user's own asset");

        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        let payload = AssetPayload {
            kind: DrawnKind::Sprite,
            name: "sprite-1".to_string(),
            bytes: sprite_bytes(&sprite),
        };
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[payload]).expect("export");

        let report = import_from(&path, &target).expect("import");
        assert!(report.imported.is_empty(), "nothing may be overwritten");
        assert_eq!(report.skipped.len(), 1);
        assert!(
            report.skipped[0].contains("sprite-1"),
            "the report must name the asset: {:?}",
            report.skipped
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_users_own_assignment_wins() {
        let root = temp_dir("conflict");
        let target = target_dirs(&root);
        // The user has assigned small_1 to their own asset.
        let mut cfg = Config::default();
        cfg.assignments.set(
            AssignmentSlot::parse("small_1").expect("small_1"),
            Some("mine".to_string()),
        );
        cfg.save_to(&target.config_file).expect("write the config");

        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest
            .assignments
            .insert("small_1".to_string(), "theirs".to_string());
        manifest
            .assignments
            .insert("small_2".to_string(), "theirs".to_string());
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[]).expect("export");

        let report = import_from(&path, &target).expect("import");
        assert_eq!(report.merged_assignments, 1, "only the free slot is filled");
        assert_eq!(report.conflicts.len(), 1);
        assert!(
            report.conflicts[0].contains("small_1"),
            "the conflict must name the slot: {:?}",
            report.conflicts
        );

        let cfg = Config::load_unsanitized_from(&target.config_file).expect("the config");
        assert_eq!(cfg.assignments.small(1), Some("mine"), "the user wins");
        assert_eq!(
            cfg.assignments.small(2),
            Some("theirs"),
            "the free slot is filled"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_import_never_removes_an_existing_assignment() {
        let root = temp_dir("keep");
        let target = target_dirs(&root);
        let mut cfg = Config::default();
        cfg.assignments.set(
            AssignmentSlot::parse("big_1").expect("big_1"),
            Some("mine".to_string()),
        );
        cfg.save_to(&target.config_file).expect("write the config");

        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest
            .assignments
            .insert("small_9".to_string(), "theirs".to_string());
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[]).expect("export");
        import_from(&path, &target).expect("import");

        let cfg = Config::load_unsanitized_from(&target.config_file).expect("the config");
        assert_eq!(cfg.assignments.big(1), Some("mine"), "the old one survives");
        assert_eq!(cfg.assignments.small(9), Some("theirs"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_import_keeps_the_users_comments_and_unknown_keys() {
        let root = temp_dir("comments");
        let target = target_dirs(&root);
        std::fs::write(
            &target.config_file,
            "# my ducks\nloop_seconds = 12.0\nmy_own_key = \"keep me\"\n\n[assignments]\nbig_1 = \"mine\"\n",
        )
        .expect("write the config");

        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest
            .assignments
            .insert("small_1".to_string(), "theirs".to_string());
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[]).expect("export");
        import_from(&path, &target).expect("import");

        let after = std::fs::read_to_string(&target.config_file).expect("read back");
        assert!(
            after.contains("# my ducks"),
            "the comment survives: {after}"
        );
        assert!(
            after.contains("my_own_key"),
            "the unknown key survives: {after}"
        );
        assert!(
            after.contains("small_1 = \"theirs\""),
            "the new slot lands: {after}"
        );
        assert!(
            after.contains("big_1 = \"mine\""),
            "the old slot stays: {after}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_assignment_to_an_asset_that_was_not_imported_is_kept() {
        let root = temp_dir("pending");
        let target = target_dirs(&root);
        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest
            .assignments
            .insert("small_1".to_string(), "later".to_string());
        let path = root.join("out.agnosis");
        export_to(&path, &manifest, &[]).expect("export");
        let report = import_from(&path, &target).expect("import");
        assert_eq!(report.merged_assignments, 1);
        let cfg = Config::load_unsanitized_from(&target.config_file).expect("the config");
        assert_eq!(
            cfg.assignments.small(1),
            Some("later"),
            "the assignment may wait for its asset"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_manifest_without_the_payload_is_refused_on_export() {
        let root = temp_dir("mismatch");
        let mut manifest = Manifest::from_config(&Config::default(), "my pack");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        let path = root.join("out.agnosis");
        let err = export_to(&path, &manifest, &[]).expect_err("the mismatch is refused");
        assert!(
            matches!(err, PackageError::MissingDocument { .. }),
            "{err:?}"
        );

        // A payload the manifest does not describe is refused too.
        let manifest = Manifest::from_config(&Config::default(), "my pack");
        let payload = AssetPayload {
            kind: DrawnKind::Sprite,
            name: "sprite-1".to_string(),
            bytes: Vec::new(),
        };
        let err = export_to(&path, &manifest, &[payload]).expect_err("the mismatch is refused");
        assert!(
            matches!(err, PackageError::UndescribedAsset { .. }),
            "{err:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_corrupt_package_is_refused_on_import() {
        let root = temp_dir("corrupt");
        let target = target_dirs(&root);
        let path = root.join("out.agnosis");
        std::fs::write(&path, b"this is not a package").expect("write");
        let err = import_from(&path, &target).expect_err("a corrupt package is refused");
        assert!(matches!(err, PackageError::Zip(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_payloads_of_the_folders_are_read_back() {
        let root = temp_dir("payloads");
        let target = target_dirs(&root);
        let sprite = small_sprite();
        crate::drawn::save_to(&target.sprites, &sprite, DrawnKind::Sprite, "sprite-1")
            .expect("save");
        let payloads = all_payloads(&target).expect("payloads");
        assert_eq!(payloads.len(), 1);
        assert_eq!(payloads[0].kind, DrawnKind::Sprite);
        assert_eq!(payloads[0].name, "sprite-1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_whole_collection_exports_in_one_file() {
        let root = temp_dir("export-all");
        let target = target_dirs(&root);
        let sprite = small_sprite();
        crate::drawn::save_to(&target.sprites, &sprite, DrawnKind::Sprite, "sprite-1")
            .expect("save");
        let mut cfg = Config::default();
        cfg.assignments.set(
            AssignmentSlot::parse("small_1").expect("small_1"),
            Some("sprite-1".to_string()),
        );
        let path = root.join("out.agnosis");
        export_all(&path, &cfg, "ducks package", &target).expect("export");

        // The package carries the asset and the assignment.
        let bytes = std::fs::read(&path).expect("read");
        let archive = ZipArchive::parse(bytes).expect("parse");
        let manifest_bytes = archive.read(MANIFEST_ENTRY).expect("the manifest");
        let text = std::str::from_utf8(manifest_bytes).expect("utf-8");
        let manifest = Manifest::from_json(text).expect("the manifest");
        assert_eq!(manifest.assets.len(), 1);
        assert_eq!(manifest.assets[0].name, "sprite-1");
        assert_eq!(
            manifest.assignments.get("small_1").map(String::as_str),
            Some("sprite-1")
        );

        // And it imports into another machine's folders.
        let other_root = temp_dir("export-all-target");
        let other = target_dirs(&other_root);
        let report = import_from(&path, &other).expect("import");
        assert_eq!(report.imported, vec!["sprite-1".to_string()]);
        assert_eq!(report.merged_assignments, 1);
        assert!(report.conflicts.is_empty());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other_root);
    }

    /// A shared package carries the **look**, never the user's machine
    /// settings: the exported manifest's fragment holds no hotkey and no
    /// browsers keys (audit 2026-10-23 §F-046).
    #[test]
    fn the_exported_fragment_carries_no_hotkey_or_browsers() {
        let root = temp_dir("look-capture");
        let target = target_dirs(&root);
        let mut cfg = Config::default();
        cfg.hotkey.windows = "Ctrl+Alt+F9".to_string();
        cfg.hotkey.editor = "Ctrl+Alt+K".to_string();
        cfg.browsers.names = vec!["mysecretbrowser".to_string()];
        cfg.sky.time_of_day = 0.8;
        let path = root.join("out.agnosis");
        export_all(&path, &cfg, "share me", &target).expect("export");

        let bytes = std::fs::read(&path).expect("read");
        let archive = ZipArchive::parse(bytes).expect("parse");
        let manifest_bytes = archive.read(MANIFEST_ENTRY).expect("the manifest");
        let text = std::str::from_utf8(manifest_bytes).expect("utf-8");
        assert!(
            !text.contains("hotkey"),
            "the manifest must not carry the hotkey table: {text}"
        );
        assert!(
            !text.contains("browsers"),
            "the manifest must not carry the browser list: {text}"
        );
        assert!(
            !text.contains("mysecretbrowser") && !text.contains("Ctrl+Alt+F9"),
            "no private value leaks into the package: {text}"
        );

        // The look itself is still there.
        let manifest = Manifest::from_json(text).expect("the manifest");
        let fragment = manifest.config().expect("the fragment parses");
        assert_eq!(fragment.sky.time_of_day, 0.8);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `capture_look` is one policy: the look fields are taken, the
    /// machine fields are the defaults (audit 2026-10-23 §F-046).
    #[test]
    fn capture_look_takes_the_look_and_leaves_the_machine() {
        let mut cfg = Config::default();
        cfg.sky.time_of_day = 0.9;
        cfg.message.text = "HELLO".to_string();
        cfg.backdrop = vec![crate::config::BackdropConfig::default()];
        cfg.decorations.big = Some("crown".to_string());
        cfg.hotkey.windows = "Ctrl+Alt+F9".to_string();
        cfg.browsers.names = vec!["mybrowser".to_string()];
        cfg.loop_seconds = 42.0;
        let look = capture_look(&cfg);
        assert_eq!(look.sky.time_of_day, 0.9);
        assert_eq!(look.message.text, "HELLO");
        assert_eq!(look.backdrop, cfg.backdrop, "the backdrop is captured");
        assert_eq!(
            look.decorations.big.as_deref(),
            Some("crown"),
            "the decorations are captured"
        );
        assert_eq!(look.hotkey, Config::default().hotkey);
        assert_eq!(look.browsers, Config::default().browsers);
        assert_eq!(look.loop_seconds, Config::default().loop_seconds);
    }

    /// `apply_look_from_fragment` lands the look's tables surgically and
    /// in one write, keeping the user's comments, hotkey and browser list
    /// (audit 2026-10-23 §F-045).
    #[test]
    fn the_look_fragment_applies_surgically() {
        let root = temp_dir("look-apply");
        let target = target_dirs(&root);
        std::fs::write(
            &target.config_file,
            "# my ducks\nloop_seconds = 12.0\n\n[hotkey]\nwindows = \"Ctrl+Alt+F9\"\n\n[sky]\nenabled = false # dusk\n",
        )
        .expect("write the config");

        let mut fragment = Config::default();
        fragment.sky.enabled = true;
        fragment.sky.time_of_day = 0.8;
        fragment.message.text = "IMPORTED".to_string();
        apply_look_from_fragment(&target.config_file, &fragment).expect("apply the look");

        let after = std::fs::read_to_string(&target.config_file).expect("read back");
        assert!(
            after.contains("# my ducks"),
            "the comment survives: {after}"
        );
        assert!(
            after.contains("windows = \"Ctrl+Alt+F9\""),
            "the hotkey survives: {after}"
        );
        assert!(
            after.contains("enabled = true # dusk"),
            "the sky lands in place, the in-table comment survives: {after}"
        );
        let cfg = Config::load_unsanitized_from(&target.config_file).expect("read back");
        assert!(cfg.sky.enabled);
        assert_eq!(cfg.sky.time_of_day, 0.8);
        assert_eq!(cfg.message.text, "IMPORTED");
        assert_eq!(cfg.loop_seconds, 12.0, "a non-look key is untouched");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A fragment one table of which the file cannot carry is refused
    /// **whole**: no half-applied look (audit 2026-10-23 §F-045/F-051
    /// discipline).
    #[test]
    fn a_look_that_cannot_splice_applies_nothing() {
        let root = temp_dir("look-refuse");
        let target = target_dirs(&root);
        // A top-level inline `grass` key: a `[grass]` section cannot
        // replace it, so the look's grass splice is impossible.
        std::fs::write(
            &target.config_file,
            "grass = { tile = \"x\" }\nloop_seconds = 12.0\n",
        )
        .expect("write the config");
        let before = std::fs::read_to_string(&target.config_file).expect("read back");

        let mut fragment = Config::default();
        fragment.sky.enabled = true;
        fragment.message.text = "HALF".to_string();
        let err =
            apply_look_from_fragment(&target.config_file, &fragment).expect_err("refused whole");

        assert!(
            matches!(err, crate::config::ConfigError::TableWrite { .. }),
            "{err:?}"
        );
        let after = std::fs::read_to_string(&target.config_file).expect("read back");
        assert_eq!(before, after, "nothing at all may land: {after}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The report summary stays renderable (test-only since §F-063).
    #[test]
    fn the_report_renders_a_summary_in_tests() {
        let report = ImportReport {
            imported: vec!["sprite-1".to_string()],
            ..ImportReport::default()
        };
        assert!(report.summary().contains("IMPORTED 1"));
    }
}

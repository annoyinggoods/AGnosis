//! Shared configuration types.
//!
//! This module is the integration contract for every other module in the crate.
//! Public field names and defaults must not change without updating all callers.
//!
//! Stage 8 adds the `[assignments]` table (roadmap §3.8, decision D11):
//! sixteen explicit keys (`big_1` .. `big_2`, `small_1` .. `small_9`,
//! `balloon_1` .. `balloon_5`) that point a parade slot at a user-drawn
//! asset. The keys are explicit so they are self-documenting in TOML and
//! easy to repair: `sanitized()` drops an empty name and a name that no
//! drawn asset carries, and reports every repair through
//! [`crate::trace::warn`]. The "does it exist?" check is injected, so the
//! unit tests never depend on the machine they run on.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::drawn::DrawnKind;

/// The bounds the **repair pass** enforces, as named constants.
///
/// WHY they are public: the editor's settings panel writes values into
/// the user's config, and a value the repair pass immediately rewrites is
/// a value the panel must not offer — the slider jumps back under the
/// user and the panel shows a number the app refuses to honour. The
/// panel reads these constants, so the two cannot drift (audit
/// 2026-10-21 §F7).
///
/// The **relative** bounds (`rope_min_big` against `rope_min_small`,
/// `rope_max` against `rope_min_big`) are not constants: they are
/// relations between two values of the same config, and the panel
/// enforces them with a cross-row clamp (see
/// [`crate::editor::clamp_setting`]).
pub mod bounds {
    /// The smallest `max_cols` the repair pass accepts.
    ///
    /// Below it the value is a stale pre-Fix-4 cap and is migrated to
    /// [`MAX_COLS_MIGRATED`] instead.
    pub const MAX_COLS_MIN: usize = 960;
    /// The value a stale `max_cols` is migrated to.
    pub const MAX_COLS_MIGRATED: usize = 4096;
    /// The largest `max_cols` the repair pass accepts.
    pub const MAX_COLS_MAX: usize = 8192;

    /// The smallest `balloons.max` the repair pass accepts.
    pub const BALLOONS_MAX_MIN: usize = 1;
    /// The largest `balloons.max` the repair pass accepts.
    pub const BALLOONS_MAX_MAX: usize = 5;

    /// The largest `balloons.balloon_radius_rows` the repair pass accepts.
    ///
    /// The conversion allocates `(2rx+1) * (2ry+1)` cells; the cap bounds
    /// it to ~66k cells, and the converter reserves its buffers
    /// fallibly, so a refused allocation is a reported error rather than
    /// an abort. The number used to live as a bare literal in the repair
    /// (audit 2026-10-23 §F-048); it is a **bound** now, so the panel and
    /// the repair read the same one.
    pub const BALLOON_RADIUS_ROWS_MAX: u32 = 128;

    /// The smallest `audio.strip_steps` the repair pass accepts.
    ///
    /// A strip of zero steps never reaches the screen.
    pub const STRIP_STEPS_MIN: usize = 1;
    /// The largest `audio.strip_steps` the repair pass accepts.
    ///
    /// A strip longer than the crossing window repeats before the train
    /// is out; 4096 is far past any monitor and keeps the editor's strip
    /// scroll bounded.
    pub const STRIP_STEPS_MAX: usize = 4096;

    /// The `max_cols` floor the row-budget grid never goes below.
    ///
    /// The cap is a perf safety valve, but a cap below the 1080p grid
    /// (960 cols at the 2x3 cell) would truncate the parade on every
    /// common monitor — so the effective cap is never smaller than this.
    pub const MAX_COLS_FLOOR: usize = 8;

    /// The smallest `fps_cap` the repair pass accepts.
    pub const FPS_CAP_MIN: u32 = 1;
    /// The largest `fps_cap` the repair pass accepts.
    pub const FPS_CAP_MAX: u32 = 240;
    /// The `fps_cap` a broken one is repaired to.
    pub const FPS_CAP_DEFAULT: u32 = 30;

    // --- The fallback values -------------------------------------------
    //
    // WHY they live here: the repair pass replaces a broken value with
    // the **default**, so a hard-coded literal in the repair was a
    // second copy of the `Default` impl's number — and a change to the
    // default silently left the repair on the old literal. Both read
    // these constants now, and `the_fallback_values_are_the_defaults`
    // pins them (audit 2026-10-21 §E4).

    /// The default `loop_seconds`.
    pub const LOOP_SECONDS_DEFAULT: f32 = 20.0;
    /// The smallest `loop_seconds` the repair pass accepts (exclusive).
    ///
    /// A loop at or below it is a divide-by-near-zero march speed, so the
    /// repair replaces such a value with [`LOOP_SECONDS_DEFAULT`].
    pub const LOOP_SECONDS_MIN: f32 = 0.1;
    /// The largest `loop_seconds` the repair pass accepts.
    ///
    /// The scene's own `advance` caps a single catch-up at 600 s; a loop
    /// slower than that would spend whole frames without the train ever
    /// visibly moving, so the value is clamped down to it.
    pub const LOOP_SECONDS_MAX: f32 = 600.0;
    /// The default cell width, in pixels.
    pub const CELL_WIDTH_DEFAULT: u32 = 12;
    /// The default cell height, in pixels.
    pub const CELL_HEIGHT_DEFAULT: u32 = 24;
    /// The default balloon radius, in grid rows.
    pub const BALLOON_RADIUS_ROWS_DEFAULT: u32 = 36;
    /// The default `balloons.rope_min_small`.
    pub const ROPE_MIN_SMALL_DEFAULT: u32 = 55;
    /// The default `balloons.pendulum_g`.
    pub const PENDULUM_G_DEFAULT: f32 = 9.8;
    /// The default `balloons.pendulum_damp`.
    pub const PENDULUM_DAMP_DEFAULT: f32 = 0.35;
    /// The default `balloons.pendulum_period`.
    pub const PENDULUM_PERIOD_DEFAULT: f32 = 1.6;
    /// The default `balloons.pendulum_drive`.
    ///
    /// The drive amplitude that keeps the swing alive at the default
    /// gravity and damping. The value used to live as a bare literal in
    /// `BalloonsConfig::default` **and** in the repair (audit 2026-10-23
    /// §F-048).
    pub const PENDULUM_DRIVE_DEFAULT: f32 = 0.9;
    /// The smallest `balloons.pendulum_drive` the repair pass accepts.
    ///
    /// Zero is legal (the drive is simply off); a negative drive pumps
    /// the pendulum backwards, so below the floor the repair falls back
    /// to the default.
    pub const PENDULUM_DRIVE_MIN: f32 = 0.0;
    /// The largest `balloons.pendulum_drive` the repair pass accepts.
    ///
    /// Above it the driven swing overshoots the balloon's rope geometry;
    /// the repair clamps the value down to it.
    pub const PENDULUM_DRIVE_MAX: f32 = 4.0;
    /// The default `message.fade_seconds`.
    pub const MESSAGE_FADE_SECONDS_DEFAULT: f32 = 8.0;
    /// The default `audio.volume`.
    pub const AUDIO_VOLUME_DEFAULT: f32 = 0.7;

    /// The smallest `balloons.rope_min_small` the repair pass accepts.
    ///
    /// Zero is not a rope: the repair pass replaces it with the default.
    pub const ROPE_MIN_SMALL_MIN: u32 = 1;
    /// The largest `balloons.rope_min_small` the panel offers.
    pub const ROPE_MIN_SMALL_MAX: u32 = 400;
    /// The largest `balloons.rope_min_big` the panel offers.
    pub const ROPE_MIN_BIG_MAX: u32 = 400;
    /// The largest `balloons.rope_max` the panel offers.
    pub const ROPE_MAX_MAX: u32 = 600;

    /// The **exclusive** lower bound of `balloons.pendulum_g`: the value
    /// must be finite and **greater than** this.
    ///
    /// A zero gravity makes the pendulum's period infinite, so the
    /// repair pass refuses it.
    pub const PENDULUM_G_EXCLUSIVE_MIN: f32 = 0.0;
    /// The floor the settings panel offers for `balloons.pendulum_g`.
    ///
    /// It is strictly above [`PENDULUM_G_EXCLUSIVE_MIN`], so every value
    /// the panel can write survives the repair pass.
    pub const PENDULUM_G_MIN: f32 = 0.1;
}

/// Which monitor the popup covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MonitorChoice {
    /// Monitor under the pointer at hotkey time.
    #[default]
    Cursor,
    /// Primary monitor.
    Primary,
}

impl MonitorChoice {
    /// Parse a monitor name. Test-only: production reads the variant from
    /// the TOML config through serde (`rename_all = "lowercase"`), so this
    /// helper would otherwise be dead code in the release build.
    #[cfg(any(test, feature = "test-support"))]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cursor" => Some(Self::Cursor),
            "primary" => Some(Self::Primary),
            _ => None,
        }
    }
}

/// Corner the exit hint is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum HintCorner {
    #[default]
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BalloonsConfig {
    /// Ring-buffer size. The plan fixes this at 5.
    pub max: usize,
    /// Balloon radius in grid ROWS; the column radius derives from the cell
    /// aspect so the disc reads circular (audit 2026-10-06 §4 consequence 4:
    /// 36 rows = 54 cols at the [2,3] cell).
    pub balloon_radius_rows: u32,
    /// Randomized rope-length bounds in rows (audit addendum note 7). A
    /// big-duck carrier draws from `rope_min_big` (head clearance), a small
    /// carrier from `rope_min_small`; both run to `rope_max`.
    pub rope_min_small: u32,
    pub rope_min_big: u32,
    pub rope_max: u32,
    pub pendulum_g: f32,
    pub pendulum_damp: f32,
    pub pendulum_drive: f32,
    pub pendulum_period: f32,
}

impl Default for BalloonsConfig {
    fn default() -> Self {
        Self {
            max: bounds::BALLOONS_MAX_MAX,
            balloon_radius_rows: bounds::BALLOON_RADIUS_ROWS_DEFAULT,
            rope_min_small: bounds::ROPE_MIN_SMALL_DEFAULT,
            rope_min_big: 85,
            rope_max: 150,
            pendulum_g: bounds::PENDULUM_G_DEFAULT,
            pendulum_damp: bounds::PENDULUM_DAMP_DEFAULT,
            pendulum_drive: bounds::PENDULUM_DRIVE_DEFAULT,
            pendulum_period: bounds::PENDULUM_PERIOD_DEFAULT,
        }
    }
}

/// The global hotkey chords.
///
/// `windows` / `linux` / `macos` are the parade chord — one string per
/// platform, because the *backends* differ. `editor` is the draw-your-own
/// chord: one string for every platform, because the chord itself does not
/// (roadmap D9). An empty `editor` string disables the editor chord.
///
/// The platform's own chord is picked by the **shell**, not by this model:
/// the *value* is the user's, but the *key* it is read from is the
/// platform's, so the choice belongs to the seam between the two halves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    pub windows: String,
    pub macos: String,
    pub linux: String,
    /// Chord that opens the draw-your-own editor.
    pub editor: String,
}

impl HotkeyConfig {
    /// Chord spec of the draw-your-own editor.
    ///
    /// One string for all platforms: the per-platform parade strings exist
    /// because the backends differ, not because the chords do (roadmap D9).
    pub fn editor_spec(&self) -> &str {
        &self.editor
    }
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            windows: "A+G+Shift+Space".to_string(),
            macos: "Ctrl+Alt+Shift+Space".to_string(),
            linux: "A+G+Shift+Space".to_string(),
            // Ctrl keeps the chord out of the typing path: plain `Shift+D` fires
            // while the user types a capital-D **name** in the parade's input
            // line, spawning the editor over the parade
            // (audit 2026-10-22 §UX-11).
            editor: "Ctrl+Shift+D".to_string(),
        }
    }
}

/// Browser process names minimized on hotkey. Empty means the built-in list.
///
/// The derived `Default` is the product decision: `names` empty (the
/// built-in list) and `restore_on_close` **off** (the joke).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BrowsersConfig {
    pub names: Vec<String>,
    /// Put the minimized browsers **back** when the parade ends.
    ///
    /// **Off by default.** The app's premise is the old Windows XP "hide
    /// your browser behind a fake hacker terminal", and that joke only
    /// works if the browser is *still hidden* when the parade is over —
    /// restoring it is the "parent walked in on you" moment that ends it.
    ///
    /// Nothing is ever **closed**, only minimized, so no work is lost
    /// either way; the user un-minimizes the browsers themselves. Turning
    /// this on is the escape hatch for anyone who would rather have the
    /// window state back automatically.
    pub restore_on_close: bool,
}

impl BrowsersConfig {
    /// Effective list, lowercased, `.exe` stripped.
    pub fn effective(&self) -> Vec<String> {
        if self.names.is_empty() {
            DEFAULT_BROWSERS.iter().map(|s| s.to_string()).collect()
        } else {
            self.names
                .iter()
                .map(|s| {
                    s.trim()
                        .to_ascii_lowercase()
                        .trim_end_matches(".exe")
                        .to_string()
                })
                .filter(|s| !s.is_empty())
                .collect()
        }
    }
}

/// Built-in browser process names, matched exactly.
///
/// The names are the EXECUTABLE names, not product names: the Tor Browser
/// ships as `torbrowser.exe` (Windows) / `torbrowser` (Linux), so it needs
/// its own entry — a bare `tor` never matches anything. On macOS the app is
/// recognized by its bundle id instead (the platform layer owns that
/// list).
pub const DEFAULT_BROWSERS: &[&str] = &[
    "chrome",
    "chromium",
    "msedge",
    "microsoft-edge",
    "microsoft-edge-stable",
    "firefox",
    "waterfox",
    "librewolf",
    "seamonkey",
    "brave",
    "brave-browser",
    "vivaldi",
    "vivaldi-bin",
    "opera",
    "operagx",
    "yandex",
    "browser",
    "thunderbird",
    "tor",
    "torbrowser",
    "epiphany",
    "konqueror",
    "comodo",
    "comodo.dragon",
    "torch",
    "thorium",
    "iron",
    "avast",
    "avgbrowser",
    "aswbrowser",
    "avg",
];

/// The kind of parade slot an assignment targets.
///
/// The three kinds mirror the roadmap's three pickers (decision D11): the
/// two lead (big) ducks, the nine trailing (small) ducks and the five
/// balloon holders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// One of the two lead ducks (150 x 70 cells).
    Big,
    /// One of the nine trailing ducks (75 x 35 cells).
    Small,
    /// One of the five balloon holders.
    Balloon,
}

impl SlotKind {
    /// The config-key prefix of this kind (`big`, `small`, `balloon`).
    pub fn prefix(&self) -> &'static str {
        match self {
            SlotKind::Big => "big",
            SlotKind::Small => "small",
            SlotKind::Balloon => "balloon",
        }
    }

    /// How many slots of this kind the parade has.
    ///
    /// The counts are derived from the parade's own tables, so a slot can
    /// never exist that the parade does not draw.
    pub fn count(&self) -> usize {
        match self {
            SlotKind::Big => crate::balloon::BIG_DUCK_SLOTS.len(),
            SlotKind::Small => crate::parade::DUCK_COUNT - crate::parade::BIG_SLOTS,
            SlotKind::Balloon => crate::balloon::BALLOON_HOLDER_DUCK.len(),
        }
    }

    /// The kind of drawn asset this slot holds: a duck slot holds a sprite,
    /// a balloon slot holds a balloon.
    fn drawn_kind(&self) -> DrawnKind {
        match self {
            SlotKind::Big | SlotKind::Small => DrawnKind::Sprite,
            SlotKind::Balloon => DrawnKind::Balloon,
        }
    }
}

/// A parsed assignment slot: the kind and the 1-based number inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignmentSlot {
    /// Big duck, small duck or balloon.
    pub kind: SlotKind,
    /// The 1-based number inside the kind (`small_3` is number 3).
    pub number: usize,
}

impl AssignmentSlot {
    /// Parse a slot name: `BIG1`..`BIG2`, `SMALL1`..`SMALL9`,
    /// `BALLOON1`..`BALLOON5`.
    ///
    /// The name is read case-insensitively and the `_`/`-` separator is
    /// optional, so `small_3`, `SMALL3` and `small-3` all name the same
    /// slot — the roadmap's examples use both spellings.
    pub fn parse(raw: &str) -> Result<Self, SlotError> {
        let key = normalise_slot_key(raw);
        let Some((kind, number)) = split_slot_key(&key) else {
            return Err(SlotError::Unknown {
                raw: raw.to_string(),
            });
        };
        if number < 1 || number > kind.count() {
            return Err(SlotError::OutOfRange {
                raw: raw.to_string(),
            });
        }
        Ok(Self { kind, number })
    }

    /// The config key of this slot (`small_3`).
    pub fn key(&self) -> String {
        format!("{}_{}", self.kind.prefix(), self.number)
    }
}

/// A slot name that cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum SlotError {
    /// The name is not a slot at all.
    #[error("unknown slot {raw:?}: use BIG1..BIG2, SMALL1..SMALL9 or BALLOON1..BALLOON5")]
    Unknown { raw: String },
    /// The name is a slot, but its number is out of range.
    #[error("slot {raw:?} is out of range: use BIG1..BIG2, SMALL1..SMALL9 or BALLOON1..BALLOON5")]
    OutOfRange { raw: String },
}

/// Normalise a typed slot name: keep the letters and digits, upper-case
/// them, drop the separators. `small_3` and `SMALL3` become the same key.
fn normalise_slot_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Split a normalised slot key into its kind and its number.
fn split_slot_key(key: &str) -> Option<(SlotKind, usize)> {
    let (kind, rest) = if let Some(rest) = key.strip_prefix("BALLOON") {
        (SlotKind::Balloon, rest)
    } else if let Some(rest) = key.strip_prefix("SMALL") {
        (SlotKind::Small, rest)
    } else if let Some(rest) = key.strip_prefix("BIG") {
        (SlotKind::Big, rest)
    } else {
        return None;
    };
    let number = rest.parse::<usize>().ok()?;
    Some((kind, number))
}

/// Every assignment slot, in config order: two big ducks, nine small ducks
/// and five balloons.
const ALL_SLOTS: [(SlotKind, usize); 16] = [
    (SlotKind::Big, 1),
    (SlotKind::Big, 2),
    (SlotKind::Small, 1),
    (SlotKind::Small, 2),
    (SlotKind::Small, 3),
    (SlotKind::Small, 4),
    (SlotKind::Small, 5),
    (SlotKind::Small, 6),
    (SlotKind::Small, 7),
    (SlotKind::Small, 8),
    (SlotKind::Small, 9),
    (SlotKind::Balloon, 1),
    (SlotKind::Balloon, 2),
    (SlotKind::Balloon, 3),
    (SlotKind::Balloon, 4),
    (SlotKind::Balloon, 5),
];

/// The `[assignments]` table: which drawn asset replaces which built-in art.
///
/// The keys are explicit (decision D11): `big_1` .. `big_2`, `small_1` ..
/// `small_9` and `balloon_1` .. `balloon_5`. A missing or empty value means
/// "no override", so the built-in art stays.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssignmentsConfig {
    /// The first big duck (train slot 0).
    pub big_1: Option<String>,
    /// The second big duck (train slot 1).
    pub big_2: Option<String>,
    /// The first small duck (train slot 2).
    pub small_1: Option<String>,
    /// The second small duck (train slot 3).
    pub small_2: Option<String>,
    /// The third small duck (train slot 4).
    pub small_3: Option<String>,
    /// The fourth small duck (train slot 5).
    pub small_4: Option<String>,
    /// The fifth small duck (train slot 6).
    pub small_5: Option<String>,
    /// The sixth small duck (train slot 7).
    pub small_6: Option<String>,
    /// The seventh small duck (train slot 8).
    pub small_7: Option<String>,
    /// The eighth small duck (train slot 9).
    pub small_8: Option<String>,
    /// The ninth small duck (train slot 10).
    pub small_9: Option<String>,
    /// The first balloon (rides the first big duck).
    pub balloon_1: Option<String>,
    /// The second balloon (rides the first small duck).
    pub balloon_2: Option<String>,
    /// The third balloon (rides the third small duck).
    pub balloon_3: Option<String>,
    /// The fourth balloon (rides the fifth small duck).
    pub balloon_4: Option<String>,
    /// The fifth balloon (rides the seventh small duck).
    pub balloon_5: Option<String>,
}

impl AssignmentsConfig {
    /// The drawn asset of the `n`-th big duck (1-based), if any.
    pub fn big(&self, n: usize) -> Option<&str> {
        self.field(SlotKind::Big, n).and_then(|v| v.as_deref())
    }

    /// The drawn asset of the `n`-th small duck (1-based), if any.
    pub fn small(&self, n: usize) -> Option<&str> {
        self.field(SlotKind::Small, n).and_then(|v| v.as_deref())
    }

    /// The drawn asset of the `n`-th balloon (1-based), if any.
    pub fn balloon(&self, n: usize) -> Option<&str> {
        self.field(SlotKind::Balloon, n).and_then(|v| v.as_deref())
    }

    /// The value of one slot, `None` when the slot is out of range.
    fn field(&self, kind: SlotKind, number: usize) -> Option<&Option<String>> {
        match (kind, number) {
            (SlotKind::Big, 1) => Some(&self.big_1),
            (SlotKind::Big, 2) => Some(&self.big_2),
            (SlotKind::Small, 1) => Some(&self.small_1),
            (SlotKind::Small, 2) => Some(&self.small_2),
            (SlotKind::Small, 3) => Some(&self.small_3),
            (SlotKind::Small, 4) => Some(&self.small_4),
            (SlotKind::Small, 5) => Some(&self.small_5),
            (SlotKind::Small, 6) => Some(&self.small_6),
            (SlotKind::Small, 7) => Some(&self.small_7),
            (SlotKind::Small, 8) => Some(&self.small_8),
            (SlotKind::Small, 9) => Some(&self.small_9),
            (SlotKind::Balloon, 1) => Some(&self.balloon_1),
            (SlotKind::Balloon, 2) => Some(&self.balloon_2),
            (SlotKind::Balloon, 3) => Some(&self.balloon_3),
            (SlotKind::Balloon, 4) => Some(&self.balloon_4),
            (SlotKind::Balloon, 5) => Some(&self.balloon_5),
            _ => None,
        }
    }

    /// Every slot, in config order, with its value.
    ///
    /// The order is the config's own (two big, nine small, five
    /// balloons), so a rendered table always lists the slots the way the
    /// config file does.
    pub fn entries(&self) -> Vec<(String, Option<&str>)> {
        ALL_SLOTS
            .iter()
            .map(|(kind, number)| {
                let key = format!("{}_{}", kind.prefix(), number);
                let value = self.field(*kind, *number).and_then(|v| v.as_deref());
                (key, value)
            })
            .collect()
    }

    /// The value of one slot, mutable; `None` when the slot is out of range.
    fn field_mut(&mut self, kind: SlotKind, number: usize) -> Option<&mut Option<String>> {
        match (kind, number) {
            (SlotKind::Big, 1) => Some(&mut self.big_1),
            (SlotKind::Big, 2) => Some(&mut self.big_2),
            (SlotKind::Small, 1) => Some(&mut self.small_1),
            (SlotKind::Small, 2) => Some(&mut self.small_2),
            (SlotKind::Small, 3) => Some(&mut self.small_3),
            (SlotKind::Small, 4) => Some(&mut self.small_4),
            (SlotKind::Small, 5) => Some(&mut self.small_5),
            (SlotKind::Small, 6) => Some(&mut self.small_6),
            (SlotKind::Small, 7) => Some(&mut self.small_7),
            (SlotKind::Small, 8) => Some(&mut self.small_8),
            (SlotKind::Small, 9) => Some(&mut self.small_9),
            (SlotKind::Balloon, 1) => Some(&mut self.balloon_1),
            (SlotKind::Balloon, 2) => Some(&mut self.balloon_2),
            (SlotKind::Balloon, 3) => Some(&mut self.balloon_3),
            (SlotKind::Balloon, 4) => Some(&mut self.balloon_4),
            (SlotKind::Balloon, 5) => Some(&mut self.balloon_5),
            _ => None,
        }
    }

    /// Set (or clear) one slot. An out-of-range slot is ignored.
    pub fn set(&mut self, slot: AssignmentSlot, name: Option<String>) {
        let Some(field) = self.field_mut(slot.kind, slot.number) else {
            return;
        };
        *field = name;
    }

    /// Drop the invalid assignments and warn about every repair.
    ///
    /// The existence check is injected: the production predicate reads the
    /// user's drawn-asset folders, the tests inject a stub, so a unit test
    /// never depends on the machine it runs on.
    pub fn sanitized_with(&mut self, exists: impl Fn(DrawnKind, &str) -> bool) {
        for (kind, number) in ALL_SLOTS {
            self.sanitize_slot(kind, number, &exists);
        }
    }

    /// Repair one slot: an empty name is dropped, a name that no drawn
    /// asset carries is dropped with a warning (the file may appear later,
    /// so this is a soft repair, never a hard error).
    fn sanitize_slot(
        &mut self,
        kind: SlotKind,
        number: usize,
        exists: &impl Fn(DrawnKind, &str) -> bool,
    ) {
        let Some(field) = self.field_mut(kind, number) else {
            return;
        };
        let Some(raw) = field.clone() else {
            return;
        };
        let trimmed = raw.trim().to_string();
        let label = format!("{}_{}", kind.prefix(), number);
        if trimmed.is_empty() {
            // An empty name is "no override" — the same as an empty
            // grass tile or a blank track name: the value is dropped
            // **silently**, with no warning and no repair (audit
            // 2026-10-21 §F10).
            *field = None;
            return;
        }
        if !exists(kind.drawn_kind(), &trimmed) {
            crate::trace::warn(&format!(
                "config assignments: {trimmed:?} in slot {label} does not exist, dropped"
            ));
            *field = None;
            return;
        }
        *field = Some(trimmed);
    }
}

/// Record a repaired personality value.
///
/// The personality is an **array of tables**, so the surgical writer
/// cannot address one entry's key: the repair is recorded under the
/// array's name and the key, and the write is refused when the file
/// cannot carry it (audit 2026-10-21 §E4).
fn record_personality_float(repairs: &mut Vec<Repair>, entry: usize, key: &str, value: f64) {
    repairs.push(Repair {
        section: "personality",
        key: format!("#{entry}.{key}"),
        // The TOML rendering, not the plain one: a float must be written
        // as a float (`bob = 1` is an **integer** in TOML, and the
        // config's `bob` is an `f32`).
        value: toml::Value::Float(value).to_string(),
    });
}

/// Where the custom message is drawn on the parade.
///
/// The default is centred above the grass: a message in a corner reads as
/// another HUD element, a message in the middle reads as part of the scene
/// (roadmap Extension 1 §3.2, decision).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessagePosition {
    /// Centred, two rows above the grass.
    #[default]
    AboveGrass,
    /// In the bottom-left corner, on its own row.
    BottomLeft,
    /// In the bottom-right corner, on its own row.
    BottomRight,
}

/// The custom on-screen message (roadmap Extension 1 §1.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageConfig {
    /// The text. Upper-cased and restricted to the block font's character
    /// set; an unsupported character is dropped and reported.
    pub text: String,
    /// Where the message is drawn.
    pub position: MessagePosition,
    /// How long the message stays before it fades. `0.0` means "always".
    pub fade_seconds: f32,
}

impl Default for MessageConfig {
    fn default() -> Self {
        Self {
            text: String::new(),
            position: MessagePosition::default(),
            fade_seconds: bounds::MESSAGE_FADE_SECONDS_DEFAULT,
        }
    }
}

/// The procedural sky gradient (roadmap Extension 1 §3.1a).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkyConfig {
    /// Draw the sky at all. Off restores the plain black background.
    pub enabled: bool,
    /// `0.0` is midnight, `0.5` is noon. Clamped into `0.0..=1.0`.
    pub time_of_day: f32,
}

impl Default for SkyConfig {
    fn default() -> Self {
        Self {
            // On by default: the ramp, the weather's contrast and the
            // grass tint are the shipped look — the first run, the web
            // default and the selftest all show them. `enabled = false`
            // stays the user's opt-out (audit 2026-10-23 §F-049; the
            // off-by-default made the whole content system invisible).
            enabled: true,
            time_of_day: 0.35,
        }
    }
}

/// The custom grass band (roadmap Extension 1 §2.1).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GrassConfig {
    /// The drawn grass tile that replaces the built-in band. `None` keeps
    /// the built-in two-row band.
    pub tile: Option<String>,
}

/// One parallax backdrop layer (roadmap Extension 4 §1.1).
///
/// The layer is a **picture**, not a drawn asset: the picture is turned
/// into a rectangular ASCII tile that repeats across the screen and
/// scrolls behind the ducks. The scroll rate is a code constant, so the
/// user picks the art and never the rate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackdropConfig {
    /// The picture the layer is built from: a file name inside the
    /// `backdrops` folder, or an absolute path.
    pub image: String,
    /// The tile width in cells. The tile repeats across the screen.
    pub cols: usize,
    /// The tile height in cells. The band is clipped to the sky region.
    pub rows: usize,
}

impl Default for BackdropConfig {
    fn default() -> Self {
        Self {
            image: String::new(),
            cols: 160,
            rows: 18,
        }
    }
}

/// The weather (roadmap Extension 1 §3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WeatherMode {
    /// Never.
    #[default]
    Off,
    /// The fixed 60 s cycle of the weather schedule.
    Auto,
    /// One forced effect.
    Rain,
    /// One forced effect.
    Snow,
    /// One forced effect.
    Wind,
    /// One forced effect.
    Lightning,
}

impl WeatherMode {
    /// The mode's own name, for the trace lines and the CLI.
    pub fn as_str(&self) -> &'static str {
        match self {
            WeatherMode::Off => "off",
            WeatherMode::Auto => "auto",
            WeatherMode::Rain => "rain",
            WeatherMode::Snow => "snow",
            WeatherMode::Wind => "wind",
            WeatherMode::Lightning => "lightning",
        }
    }
}

/// The weather settings (roadmap Extension 1 §5.8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeatherConfig {
    /// Which weather runs.
    pub mode: WeatherMode,
    /// The particle density and the effect strength, `0.0..=2.0`.
    pub intensity: f32,
    /// The seed of the deterministic PRNG. The weather is a pure function
    /// of the clock and this seed, so the selftest gate stays byte-identical.
    pub seed: u64,
}

impl Default for WeatherConfig {
    fn default() -> Self {
        Self {
            mode: WeatherMode::default(),
            intensity: 1.0,
            seed: 1,
        }
    }
}

/// The persistent counters (roadmap Extension 1 §1.7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatsConfig {
    /// Draw the "days since the ducks" counter in the bottom-left corner.
    pub show_days: bool,
}

/// One duck's personality (roadmap Extension 1 §2.2).
///
/// A personality is a **delta**, not a replacement: `bob = 1.0` and
/// `gait_speed = 1.0` are the defaults, so the config lists only the ducks
/// that differ from the built-in art. The size class is deliberately *not*
/// a personality — the train's geometry depends on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonalityConfig {
    /// The duck this personality applies to (`big_1`, `small_3`, ...).
    pub slot: String,
    /// The bob amplitude multiplier. `1.0` is the built-in bob.
    pub bob: f32,
    /// The gait speed multiplier. It changes the **animation phase** only —
    /// never the train's spacing.
    pub gait_speed: f32,
    /// A whole gait phase added to this duck's animation.
    pub phase_offset: i32,
    /// The extra hop height in cells, at the top of the hop.
    pub hop: f32,
    /// The duck's colour, when it differs from the built-in yellow.
    pub color: Option<[u8; 3]>,
}

impl Default for PersonalityConfig {
    fn default() -> Self {
        Self {
            slot: String::new(),
            bob: 1.0,
            gait_speed: 1.0,
            phase_offset: 0,
            hop: 0.0,
            color: None,
        }
    }
}

impl PersonalityConfig {
    /// The bob amplitude in cells, from the multiplier.
    ///
    /// The built-in bob is one cell: a duck that walks one cell up and one
    /// cell down reads as a walk, and the multiplier scales that.
    pub fn bob_cells(&self) -> f32 {
        (self.bob * BUILTIN_BOB_CELLS).clamp(0.0, MAX_BOB_CELLS)
    }

    /// The train slot this personality applies to.
    ///
    /// `None` when the slot does not name a duck — a balloon slot has no
    /// duck to give a personality to.
    pub fn duck_slot(&self) -> Option<usize> {
        let slot = AssignmentSlot::parse(&self.slot).ok()?;
        match slot.kind {
            SlotKind::Big => crate::drawn::big_slot(slot.number),
            SlotKind::Small => crate::drawn::small_slot(slot.number),
            SlotKind::Balloon => None,
        }
    }
}

/// The built-in bob amplitude in cells.
pub const BUILTIN_BOB_CELLS: f32 = 1.0;

/// The default exit hint: the one line that tells the user how to leave.
///
/// WHY a named constant: the repair pass falls back to it when the
/// user's hint is blank (audit 2026-10-23 §F-065), and the fallback and
/// the `Default` impl must be the **same** text or the repair would
/// rewrite a hint the user never changed the meaning of.
pub const DEFAULT_EXIT_HINT: &str = "press ESC to exit";

/// The cap on a personality's bob amplitude, in cells.
pub const MAX_BOB_CELLS: f32 = 8.0;

/// The duck decorations: what the big ducks and the small ducks wear.
///
/// A decoration is a second frame-sized asset that is drawn **over** the
/// duck's body (roadmap Extension 2 §4). There is one name per size
/// class, so a decoration is a global style in v1 — "the ducks all wear
/// the same hat" — and not a per-duck setting.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DecorationsConfig {
    /// The decoration the big ducks wear; `None` keeps them bare.
    pub big: Option<String>,
    /// The decoration the small ducks wear; `None` keeps them bare.
    pub small: Option<String>,
}

impl DecorationsConfig {
    /// The decoration of one size class, if any.
    pub fn get(&self, size: crate::drawn::DrawnSize) -> Option<&str> {
        match size {
            crate::drawn::DrawnSize::Big => self.big.as_deref(),
            crate::drawn::DrawnSize::Small => self.small.as_deref(),
        }
    }
}

/// The default voice: the synth's triangle, the brightest of the three
/// without the hardness of the square.
pub const DEFAULT_WAVEFORM: &str = "triangle";

/// The sound settings (roadmap Extension 3 §2.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// The master gain, 0.0..=1.0. 0.0 is silence.
    pub volume: f32,
    /// Mute the whole output without losing the volume.
    pub muted: bool,
    /// The name of the track in `<config>/tracks/` that the parade plays.
    /// `None` means "no track": the parade is silent apart from the effects.
    pub track: Option<String>,
    /// The length of the note strip, in sprite steps. One step is one
    /// sprite frame, so the strip is the length of the crossing window
    /// (263 steps at the default settings).
    pub strip_steps: usize,
    /// The voice the track's notes are played in: `sine`, `triangle` or
    /// `square`.
    pub waveform: String,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            volume: bounds::AUDIO_VOLUME_DEFAULT,
            muted: false,
            track: None,
            strip_steps: 263,
            waveform: DEFAULT_WAVEFORM.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub loop_seconds: f32,
    /// Legacy: per-cell pixel size at 100% scale. Unused since the
    /// row-budget grid (audit 2026-10-06 §4.5) derived the cell from the
    /// monitor height; kept so existing configs keep loading.
    pub cell_size: [u32; 2],
    pub monitor: MonitorChoice,
    pub max_cols: usize,
    pub fps_cap: u32,
    pub exit_hint: String,
    pub hint_corner: HintCorner,
    pub balloons: BalloonsConfig,
    pub hotkey: HotkeyConfig,
    pub browsers: BrowsersConfig,
    /// Which drawn asset replaces which built-in art.
    pub assignments: AssignmentsConfig,
    /// The custom on-screen message.
    pub message: MessageConfig,
    /// The procedural sky gradient.
    pub sky: SkyConfig,
    /// The custom grass band.
    pub grass: GrassConfig,
    /// The parallax backdrop layers, back to front.
    ///
    /// The list is **omitted** when it is empty, for the same reason the
    /// personality list is: an inline `backdrop = []` key cannot live
    /// next to the `[[backdrop]]` tables the user writes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub backdrop: Vec<BackdropConfig>,
    /// The weather.
    pub weather: WeatherConfig,
    /// The persistent counters.
    pub stats: StatsConfig,
    /// The duck decorations: what the ducks wear.
    pub decorations: DecorationsConfig,
    /// The sound settings.
    pub audio: AudioConfig,
    /// The per-duck personalities. Only the ducks that differ are listed.
    ///
    /// The list is **omitted** when it is empty: an empty list would
    /// serialize as an inline `personality = []` key, and an inline key
    /// cannot live next to the `[[personality]]` tables the user writes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub personality: Vec<PersonalityConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            loop_seconds: bounds::LOOP_SECONDS_DEFAULT,
            cell_size: [bounds::CELL_WIDTH_DEFAULT, bounds::CELL_HEIGHT_DEFAULT],
            monitor: MonitorChoice::Cursor,
            max_cols: bounds::MAX_COLS_MIGRATED,
            fps_cap: bounds::FPS_CAP_DEFAULT,
            exit_hint: DEFAULT_EXIT_HINT.to_string(),
            hint_corner: HintCorner::BottomRight,
            balloons: BalloonsConfig::default(),
            hotkey: HotkeyConfig::default(),
            browsers: BrowsersConfig::default(),
            assignments: AssignmentsConfig::default(),
            message: MessageConfig::default(),
            sky: SkyConfig::default(),
            grass: GrassConfig::default(),
            backdrop: Vec::new(),
            weather: WeatherConfig::default(),
            stats: StatsConfig::default(),
            decorations: DecorationsConfig::default(),
            audio: AudioConfig::default(),
            personality: Vec::new(),
        }
    }
}

/// One value the repair pass changed.
///
/// The repair pass clamps an out-of-range value to a valid one. When the
/// config is written back, the repair is recorded here so the value can be
/// replaced **in place** in the user's file — the app's repair then sticks,
/// and the warning does not repeat on every run (audit 2026-10-20 §3.10).
#[derive(Debug, Clone, PartialEq)]
pub struct Repair {
    /// The table the key lives in; `""` is the top level.
    pub section: &'static str,
    /// The key inside the section.
    ///
    /// WHY it is a `String`: a repair of an **array entry** addresses
    /// the entry by its number — `#2.bob` is the third entry of the
    /// block, its `bob` key — and the number is not known when the
    /// repair is written (audit 2026-10-21 §E4).
    pub key: String,
    /// The repaired value, already rendered as TOML.
    pub value: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error(transparent)]
    Path(#[from] crate::path_error::PathError),
    /// The table could not be written without breaking the file.
    #[error("could not write the [{section}] table into {path}: the file would not parse")]
    TableWrite {
        /// The table that could not be written.
        section: String,
        /// The config file.
        path: PathBuf,
    },
}

impl Config {
    /// Load the config from an explicit path and repair it.
    ///
    /// The **existence predicate** is injected: the core cannot resolve a
    /// platform location, so the shell hands over the one that answers
    /// "does this drawn asset exist?". The repair pass needs it to drop
    /// an assignment whose asset is gone (a *soft* repair — the file may
    /// appear later).
    ///
    /// The repairs are written back into the file, so the clamp sticks
    /// and the warning does not repeat on every run.
    pub fn load_from(
        path: &Path,
        exists: impl Fn(DrawnKind, &str) -> bool,
    ) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let cfg: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let (cfg, repairs) = cfg.sanitized_with_repairs(exists);
        persist_repairs(path, &repairs);
        Ok(cfg)
    }

    /// Load from an explicit path **without** the repairs.
    ///
    /// The CLI edits the config the way the user wrote it: a repair pass
    /// would silently drop the assignments whose asset does not exist (yet)
    /// and rewrite unrelated keys the CLI was never asked to touch. The
    /// daemon and the popup keep using [`Config::load_from`], so the
    /// repairs still happen on every start.
    pub fn load_unsanitized_from(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Save the config into an explicit path.
    ///
    /// The CLI and the tests write into a temp dir through this seam, so a
    /// test never touches the user's real config file.
    ///
    /// The write is **atomic** and keeps a `.bak` next to the file: a
    /// truncated config fails to parse, and a truncated config used to
    /// brick every config-reading command with no way back (audit
    /// 2026-10-20 §3.9).
    pub fn save_to(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        crate::config_edit::write_atomic(path, &text)
    }

    /// Clamp values that would otherwise produce a degenerate or empty grid.
    /// Every repair is reported via [`crate::trace::warn`] so a silently
    /// "fixed" config never surprises its author (audit 2026-10-17 §Robustness).
    ///
    /// The **existence predicate** is injected: the core cannot resolve a
    /// platform location, so the shell hands over the one that answers
    /// "does this drawn asset exist?".
    ///
    /// Test-only helper: the production path is
    /// [`Config::sanitized_with_repairs`], which also reports the repairs
    /// so the load can write them back.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sanitized(self, exists: impl Fn(DrawnKind, &str) -> bool) -> Self {
        let mut repairs = Vec::new();
        self.sanitized_reporting_into(exists, &mut repairs)
    }

    /// The repair pass, reporting every value it changed.
    ///
    /// The repairs are what the load writes back into the file, so the
    /// clamp sticks and the warning does not repeat on every run (audit
    /// 2026-10-20 §3.10).
    ///
    /// The **existence predicate** is injected: the core cannot resolve a
    /// platform location, so the shell hands over the one that answers
    /// "does this drawn asset exist?".
    pub fn sanitized_with_repairs(
        self,
        exists: impl Fn(DrawnKind, &str) -> bool,
    ) -> (Self, Vec<Repair>) {
        let mut repairs = Vec::new();
        let cfg = self.sanitized_reporting_into(exists, &mut repairs);
        (cfg, repairs)
    }

    /// [`Config::sanitized_with_repairs`] with the injected predicate
    /// handed straight through.
    ///
    /// WHY it is public: the repair chain is a **public** contract —
    /// `sanitized`, `sanitized_with_repairs`, `sanitized_with_reporting`
    /// and this one — and AGENTS.md names all four.
    pub fn sanitized_reporting_into(
        self,
        exists: impl Fn(DrawnKind, &str) -> bool,
        repairs: &mut Vec<Repair>,
    ) -> Self {
        self.sanitized_with_reporting(exists, repairs)
    }

    /// The repair pass with an injected predicate, reporting the repairs.
    pub fn sanitized_with_reporting(
        mut self,
        exists: impl Fn(DrawnKind, &str) -> bool,
        repairs: &mut Vec<Repair>,
    ) -> Self {
        if !(self.loop_seconds.is_finite() && self.loop_seconds > bounds::LOOP_SECONDS_MIN) {
            crate::trace::warn(&format!(
                "config loop_seconds {} invalid, using {}",
                self.loop_seconds,
                bounds::LOOP_SECONDS_DEFAULT
            ));
            self.loop_seconds = bounds::LOOP_SECONDS_DEFAULT;
            record_float(repairs, "", "loop_seconds", self.loop_seconds as f64);
        }
        if self.loop_seconds > bounds::LOOP_SECONDS_MAX {
            let clamped = bounds::LOOP_SECONDS_MAX;
            crate::trace::warn(&format!(
                "config loop_seconds {} above {}, using {clamped}",
                self.loop_seconds,
                bounds::LOOP_SECONDS_MAX
            ));
            self.loop_seconds = clamped;
            record_float(repairs, "", "loop_seconds", self.loop_seconds as f64);
        }
        let mut cell_size_repaired = false;
        if self.cell_size[0] == 0 {
            crate::trace::warn(&format!(
                "config cell_size[0] is 0, using {}",
                bounds::CELL_WIDTH_DEFAULT
            ));
            self.cell_size[0] = bounds::CELL_WIDTH_DEFAULT;
            cell_size_repaired = true;
        }
        if self.cell_size[1] == 0 {
            crate::trace::warn(&format!(
                "config cell_size[1] is 0, using {}",
                bounds::CELL_HEIGHT_DEFAULT
            ));
            self.cell_size[1] = bounds::CELL_HEIGHT_DEFAULT;
            cell_size_repaired = true;
        }
        if cell_size_repaired {
            record_array(repairs, "", "cell_size", self.cell_size);
        }
        // The row-budget grid needs ~960 cols at 1080p (more on wider
        // panels). Pre-Fix-4 configs carry max_cols = 260, which would
        // silently truncate the parade — migrate stale caps up to the new
        // default; `max_cols` is a perf safety valve, not a layout control.
        if self.max_cols < bounds::MAX_COLS_MIN {
            crate::trace::warn(&format!(
                "config max_cols {} below {}, using {}",
                self.max_cols,
                bounds::MAX_COLS_MIN,
                bounds::MAX_COLS_MIGRATED
            ));
            self.max_cols = bounds::MAX_COLS_MIGRATED;
            record_int(repairs, "", "max_cols", self.max_cols as i64);
        }
        if self.max_cols > bounds::MAX_COLS_MAX {
            crate::trace::warn(&format!(
                "config max_cols {} above {}, using {}",
                self.max_cols,
                bounds::MAX_COLS_MAX,
                bounds::MAX_COLS_MAX
            ));
            self.max_cols = bounds::MAX_COLS_MAX;
            record_int(repairs, "", "max_cols", self.max_cols as i64);
        }
        if self.fps_cap < bounds::FPS_CAP_MIN || self.fps_cap > bounds::FPS_CAP_MAX {
            crate::trace::warn(&format!(
                "config fps_cap {} invalid, using {}",
                self.fps_cap,
                bounds::FPS_CAP_DEFAULT
            ));
            self.fps_cap = bounds::FPS_CAP_DEFAULT;
            record_int(repairs, "", "fps_cap", self.fps_cap as i64);
        }
        // Scene holds 5 ring-buffer slots: larger values render nothing extra
        // (audit 2026-10-17 §Maintainability "sanitize lies").
        if self.balloons.max < bounds::BALLOONS_MAX_MIN
            || self.balloons.max > bounds::BALLOONS_MAX_MAX
        {
            crate::trace::warn(&format!(
                "config balloons.max {} outside {}..={}, using {}",
                self.balloons.max,
                bounds::BALLOONS_MAX_MIN,
                bounds::BALLOONS_MAX_MAX,
                bounds::BALLOONS_MAX_MAX
            ));
            self.balloons.max = bounds::BALLOONS_MAX_MAX;
            record_int(repairs, "balloons", "max", self.balloons.max as i64);
        }
        // Conversion allocates (2*rx+1)*(2*ry+1) cells; fail early in config
        // instead of late in the converter (audit 2026-10-17 §Robustness).
        // The 128 cap bounds conversion to ~66k cells, and the converter
        // reserves its buffers fallibly, so a refused allocation is a
        // reported error rather than an abort.
        if self.balloons.balloon_radius_rows == 0 {
            crate::trace::warn(&format!(
                "config balloon_radius_rows is 0, using {}",
                bounds::BALLOON_RADIUS_ROWS_DEFAULT
            ));
            self.balloons.balloon_radius_rows = bounds::BALLOON_RADIUS_ROWS_DEFAULT;
            record_int(
                repairs,
                "balloons",
                "balloon_radius_rows",
                self.balloons.balloon_radius_rows as i64,
            );
        }
        if self.balloons.balloon_radius_rows > bounds::BALLOON_RADIUS_ROWS_MAX {
            crate::trace::warn(&format!(
                "config balloon_radius_rows {} above {}, using {}",
                self.balloons.balloon_radius_rows,
                bounds::BALLOON_RADIUS_ROWS_MAX,
                bounds::BALLOON_RADIUS_ROWS_MAX
            ));
            self.balloons.balloon_radius_rows = bounds::BALLOON_RADIUS_ROWS_MAX;
            record_int(
                repairs,
                "balloons",
                "balloon_radius_rows",
                self.balloons.balloon_radius_rows as i64,
            );
        }
        // Zero-repair matches the 55 default (audit 2026-10-17 §Maintainability).
        if self.balloons.rope_min_small < bounds::ROPE_MIN_SMALL_MIN {
            crate::trace::warn(&format!(
                "config rope_min_small is 0, using {}",
                bounds::ROPE_MIN_SMALL_DEFAULT
            ));
            self.balloons.rope_min_small = bounds::ROPE_MIN_SMALL_DEFAULT;
            record_int(
                repairs,
                "balloons",
                "rope_min_small",
                self.balloons.rope_min_small as i64,
            );
        }
        if self.balloons.rope_min_big < self.balloons.rope_min_small {
            crate::trace::warn(&format!(
                "config rope_min_big {} below rope_min_small {}, using {}",
                self.balloons.rope_min_big,
                self.balloons.rope_min_small,
                self.balloons.rope_min_small
            ));
            self.balloons.rope_min_big = self.balloons.rope_min_small;
            record_int(
                repairs,
                "balloons",
                "rope_min_big",
                self.balloons.rope_min_big as i64,
            );
        }
        if self.balloons.rope_max < self.balloons.rope_min_big {
            crate::trace::warn(&format!(
                "config rope_max {} below rope_min_big {}, using {}",
                self.balloons.rope_max, self.balloons.rope_min_big, self.balloons.rope_min_big
            ));
            self.balloons.rope_max = self.balloons.rope_min_big;
            record_int(
                repairs,
                "balloons",
                "rope_max",
                self.balloons.rope_max as i64,
            );
        }
        if !(self.balloons.pendulum_g.is_finite()
            && self.balloons.pendulum_g > bounds::PENDULUM_G_EXCLUSIVE_MIN)
        {
            crate::trace::warn(&format!(
                "config pendulum_g {} invalid, using {}",
                self.balloons.pendulum_g,
                bounds::PENDULUM_G_DEFAULT
            ));
            self.balloons.pendulum_g = bounds::PENDULUM_G_DEFAULT;
            record_float(
                repairs,
                "balloons",
                "pendulum_g",
                self.balloons.pendulum_g as f64,
            );
        }
        if !self.balloons.pendulum_damp.is_finite() || self.balloons.pendulum_damp < 0.0 {
            crate::trace::warn(&format!(
                "config pendulum_damp {} invalid, using {}",
                self.balloons.pendulum_damp,
                bounds::PENDULUM_DAMP_DEFAULT
            ));
            self.balloons.pendulum_damp = bounds::PENDULUM_DAMP_DEFAULT;
            record_float(
                repairs,
                "balloons",
                "pendulum_damp",
                self.balloons.pendulum_damp as f64,
            );
        }
        if !self.balloons.pendulum_drive.is_finite()
            || self.balloons.pendulum_drive < bounds::PENDULUM_DRIVE_MIN
        {
            crate::trace::warn(&format!(
                "config pendulum_drive {} invalid, using {}",
                self.balloons.pendulum_drive,
                bounds::PENDULUM_DRIVE_DEFAULT
            ));
            self.balloons.pendulum_drive = bounds::PENDULUM_DRIVE_DEFAULT;
            record_float(
                repairs,
                "balloons",
                "pendulum_drive",
                self.balloons.pendulum_drive as f64,
            );
        }
        if self.balloons.pendulum_drive > bounds::PENDULUM_DRIVE_MAX {
            let clamped = bounds::PENDULUM_DRIVE_MAX;
            crate::trace::warn(&format!(
                "config pendulum_drive {} above {}, using {clamped}",
                self.balloons.pendulum_drive,
                bounds::PENDULUM_DRIVE_MAX
            ));
            self.balloons.pendulum_drive = clamped;
            record_float(
                repairs,
                "balloons",
                "pendulum_drive",
                self.balloons.pendulum_drive as f64,
            );
        }
        if !(self.balloons.pendulum_period.is_finite() && self.balloons.pendulum_period > 0.0) {
            crate::trace::warn(&format!(
                "config pendulum_period {} invalid, using {}",
                self.balloons.pendulum_period,
                bounds::PENDULUM_PERIOD_DEFAULT
            ));
            self.balloons.pendulum_period = bounds::PENDULUM_PERIOD_DEFAULT;
            // WHY the record: without it the repair was corrected in
            // memory on every load and never written back — the warning
            // repeated forever and the file kept the broken value
            // (audit 2026-10-21 §E4).
            record_float(
                repairs,
                "balloons",
                "pendulum_period",
                self.balloons.pendulum_period as f64,
            );
        }
        self.assignments.sanitized_with(&exists);
        self.repair_message(repairs);
        self.repair_sky(repairs);
        self.repair_weather(repairs);
        self.repair_audio(repairs);
        self.repair_grass(&exists);
        self.repair_decorations(&exists);
        self.repair_backdrop();
        self.repair_personalities(repairs);
        self
    }

    /// Repair the custom message: the text is upper-cased and restricted to
    /// the block font's character set, and the fade is clamped.
    ///
    /// An unsupported character is **dropped and reported** — the rest of
    /// the message still shows (roadmap Extension 1 §3.2, AC 2). The
    /// font is the constraint: it carries only `A-Z 0-9 . : _ - / \` and
    /// space, and extending it is a separate, bigger change.
    fn repair_message(&mut self, repairs: &mut Vec<Repair>) {
        let (text, dropped) = clean_message(&self.message.text);
        for ch in dropped {
            crate::trace::warn(&format!(
                "config message: unsupported character {ch:?} dropped (the block font carries A-Z 0-9 . : _ - / \\)"
            ));
        }
        if text != self.message.text {
            self.message.text = text.clone();
            record_string(repairs, "message", "text", &text);
        }
        if !(self.message.fade_seconds.is_finite() && self.message.fade_seconds >= 0.0) {
            crate::trace::warn(&format!(
                "config message.fade_seconds {} invalid, using {}",
                self.message.fade_seconds,
                bounds::MESSAGE_FADE_SECONDS_DEFAULT
            ));
            self.message.fade_seconds = bounds::MESSAGE_FADE_SECONDS_DEFAULT;
            record_float(
                repairs,
                "message",
                "fade_seconds",
                bounds::MESSAGE_FADE_SECONDS_DEFAULT as f64,
            );
        }
        // The exit hint is block-font text too: without the same repair a
        // natural hint such as "press ESC (or F12)" lost its parentheses
        // **silently** on screen — the font skips what it cannot draw
        // (audit 2026-10-22 §UI-19). The repair only **drops**: the HUD
        // upper-cases at draw time, so the user's own spelling stays in
        // the file, and the default hint passes through untouched.
        let (hint, dropped) = clean_hint(&self.exit_hint);
        for ch in dropped {
            crate::trace::warn(&format!(
                "config exit_hint: unsupported character {ch:?} dropped (the block font carries A-Z 0-9 . : _ - / \\)"
            ));
        }
        if hint != self.exit_hint {
            self.exit_hint = hint.clone();
            record_string(repairs, "", "exit_hint", &hint);
        }
        // A hint that is **blank** — empty, spaces, or nothing the font
        // can draw at all — removes the only on-screen instruction for
        // leaving the parade, and a fullscreen window without one is a
        // trap. The repair falls back to the default hint and is recorded
        // like any other repair, so the file stops carrying a blank
        // (audit 2026-10-23 §F-065).
        if self.exit_hint.trim().is_empty() {
            crate::trace::warn(&format!(
                "config exit_hint is blank, using {DEFAULT_EXIT_HINT:?}"
            ));
            self.exit_hint = DEFAULT_EXIT_HINT.to_string();
            record_string(repairs, "", "exit_hint", DEFAULT_EXIT_HINT);
        }
    }

    /// Repair the sky's time of day: it must be a finite `0.0..=1.0`.
    fn repair_sky(&mut self, repairs: &mut Vec<Repair>) {
        if !self.sky.time_of_day.is_finite() {
            crate::trace::warn("config sky.time_of_day invalid, using 0.35");
            self.sky.time_of_day = 0.35;
            record_float(repairs, "sky", "time_of_day", 0.35);
            return;
        }
        let clamped = self.sky.time_of_day.clamp(0.0, 1.0);
        if (clamped - self.sky.time_of_day).abs() > f32::EPSILON {
            crate::trace::warn(&format!(
                "config sky.time_of_day {} outside 0.0..=1.0, using {clamped}",
                self.sky.time_of_day
            ));
            self.sky.time_of_day = clamped;
            record_float(repairs, "sky", "time_of_day", clamped as f64);
        }
    }

    /// Repair the weather's intensity: it must be a finite `0.0..=2.0`.
    fn repair_weather(&mut self, repairs: &mut Vec<Repair>) {
        if !(self.weather.intensity.is_finite() && self.weather.intensity >= 0.0) {
            crate::trace::warn(&format!(
                "config weather.intensity {} invalid, using 1.0",
                self.weather.intensity
            ));
            self.weather.intensity = 1.0;
            record_float(repairs, "weather", "intensity", 1.0);
            return;
        }
        let clamped = self.weather.intensity.min(2.0);
        if (clamped - self.weather.intensity).abs() > f32::EPSILON {
            crate::trace::warn(&format!(
                "config weather.intensity {} above 2.0, using 2.0",
                self.weather.intensity
            ));
            self.weather.intensity = clamped;
            record_float(repairs, "weather", "intensity", clamped as f64);
        }
    }

    /// Repair the sound settings: the volume must be a finite `0.0..=1.0`,
    /// the note strip a sane length and a blank track name means "no
    /// track".
    fn repair_audio(&mut self, repairs: &mut Vec<Repair>) {
        self.repair_audio_volume(repairs);
        self.repair_audio_strip(repairs);
        self.repair_audio_track();
        self.repair_audio_waveform(repairs);
    }

    /// Repair the voice: it must be one of the three the synth knows.
    ///
    /// A typo in the voice must not take the whole configuration down, so
    /// an unknown name falls back to the default and is written back.
    fn repair_audio_waveform(&mut self, repairs: &mut Vec<Repair>) {
        let name = self.audio.waveform.trim().to_ascii_lowercase();
        if crate::audio::synth::waveform_from_name(&name).is_some() {
            self.audio.waveform = name;
            return;
        }
        crate::trace::warn(&format!(
            "config audio.waveform {:?} unknown, using {DEFAULT_WAVEFORM}",
            self.audio.waveform
        ));
        self.audio.waveform = DEFAULT_WAVEFORM.to_string();
        record_string(repairs, "audio", "waveform", DEFAULT_WAVEFORM);
    }

    /// Repair the master gain: it must be a finite `0.0..=1.0`.
    fn repair_audio_volume(&mut self, repairs: &mut Vec<Repair>) {
        if !self.audio.volume.is_finite() {
            crate::trace::warn(&format!(
                "config audio.volume {} invalid, using {}",
                self.audio.volume,
                bounds::AUDIO_VOLUME_DEFAULT
            ));
            self.audio.volume = bounds::AUDIO_VOLUME_DEFAULT;
            record_float(
                repairs,
                "audio",
                "volume",
                bounds::AUDIO_VOLUME_DEFAULT as f64,
            );
            return;
        }
        let clamped = self.audio.volume.clamp(0.0, 1.0);
        if (clamped - self.audio.volume).abs() > f32::EPSILON {
            crate::trace::warn(&format!(
                "config audio.volume {} outside 0.0..=1.0, using {clamped}",
                self.audio.volume
            ));
            self.audio.volume = clamped;
            record_float(repairs, "audio", "volume", clamped as f64);
        }
    }

    /// Repair the note-strip length: a strip of zero steps never reaches
    /// the screen, and a huge one never ends.
    fn repair_audio_strip(&mut self, repairs: &mut Vec<Repair>) {
        let clamped = self
            .audio
            .strip_steps
            .clamp(bounds::STRIP_STEPS_MIN, bounds::STRIP_STEPS_MAX);
        if clamped == self.audio.strip_steps {
            return;
        }
        crate::trace::warn(&format!(
            "config audio.strip_steps {} outside {}..={}, using {clamped}",
            self.audio.strip_steps,
            bounds::STRIP_STEPS_MIN,
            bounds::STRIP_STEPS_MAX
        ));
        self.audio.strip_steps = clamped;
        record_int(repairs, "audio", "strip_steps", clamped as i64);
    }

    /// Repair the track name: a blank name means "no track".
    ///
    /// The name is written back as an empty string, so the file stops
    /// carrying a name that means nothing (the same discipline the grass
    /// and decoration repairs use).
    fn repair_audio_track(&mut self) {
        let Some(raw) = self.audio.track.clone() else {
            return;
        };
        if !raw.trim().is_empty() {
            self.audio.track = Some(raw.trim().to_string());
            return;
        }
        // A blank name is "no track" — the same as an empty grass tile
        // or an empty decoration: the value is dropped **silently**.
        //
        // WHY: the repair used to warn **and** write the empty value
        // back, so the next load read `Some("")` again and warned again
        // — the same warning, on every single load (audit 2026-10-21
        // §F10).
        self.audio.track = None;
    }

    /// Repair the grass tile: an empty name means "no override" and a name
    /// that no drawn asset carries is dropped (a **soft** repair — the
    /// asset may still appear later).
    ///
    /// The drop is **memory-only**: the name stays in the file, exactly
    /// as the assignment repair behaves. The repair used to write an
    /// empty string back, which *erased the user's pending name* — draw
    /// the grass tile tomorrow and the override that pointed at it was
    /// already gone (audit 2026-10-22 §P2-18).
    fn repair_grass(&mut self, exists: &impl Fn(DrawnKind, &str) -> bool) {
        let Some(raw) = self.grass.tile.clone() else {
            return;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // An empty name is "no override" and is not a repair at all.
            self.grass.tile = None;
            return;
        }
        if !exists(DrawnKind::Grass, trimmed) {
            crate::trace::warn(&format!(
                "config grass.tile {trimmed:?} does not exist, dropped (the name stays in the file)"
            ));
            self.grass.tile = None;
            return;
        }
        self.grass.tile = Some(trimmed.to_string());
    }

    /// Repair the decorations: one name per size class, each with the
    /// grass repair's **soft** policy — a name no drawn asset carries is
    /// dropped in memory only, and the file keeps it (audit 2026-10-22
    /// §P2-18 / 2026-10-23 §F-044).
    fn repair_decorations(&mut self, exists: &impl Fn(DrawnKind, &str) -> bool) {
        self.repair_decoration(&exists, true);
        self.repair_decoration(&exists, false);
    }

    /// Repair the parallax backdrop list: an empty picture name is
    /// dropped, the tile size is clamped, and a stack deeper than the
    /// layer table holds is cut.
    ///
    /// The repairs are **not written back**, and that is deliberate: the
    /// surgical writer replaces one key inside a named table, and the
    /// backdrop is an **array of tables** (`[[backdrop]]`). Rewriting the
    /// whole array would throw away the user's comments for a clamp that
    /// is harmless in memory — the loader clamps again on every load, and
    /// a missing picture is not a repair at all (the file may still
    /// appear later, so its existence is the loader's business, not the
    /// config load's).
    fn repair_backdrop(&mut self) {
        let max_rows = crate::backdrop::MAX_TILE_ROWS;
        let max_cols = crate::backdrop::MAX_TILE_COLS;
        self.backdrop.retain(|layer| !layer.image.trim().is_empty());
        self.backdrop.truncate(crate::layer::MAX_BACKDROPS);
        for layer in &mut self.backdrop {
            layer.image = layer.image.trim().to_string();
            layer.cols = layer.cols.clamp(1, max_cols);
            layer.rows = layer.rows.clamp(1, max_rows);
        }
    }

    /// Repair one decoration name: an empty name means "no override" and a
    /// name that no drawn asset carries is dropped (a **soft** repair).
    ///
    /// The drop is **memory-only** — the name stays in the file, the same
    /// discipline as the assignment and grass repairs: the asset may be
    /// drawn later, and erasing the pending name lost the user's intent
    /// (audit 2026-10-22 §P2-18).
    fn repair_decoration(&mut self, exists: &impl Fn(DrawnKind, &str) -> bool, big: bool) {
        let raw = if big {
            self.decorations.big.clone()
        } else {
            self.decorations.small.clone()
        };
        let Some(raw) = raw else {
            return;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // An empty name is "no override" and is not a repair at all.
            self.set_decoration(big, None);
            return;
        }
        if !exists(DrawnKind::Decoration, trimmed) {
            crate::trace::warn(&format!(
                "config decorations {trimmed:?} does not exist, dropped (the name stays in the file)"
            ));
            self.set_decoration(big, None);
            return;
        }
        self.set_decoration(big, Some(trimmed.to_string()));
    }

    /// Set (or clear) one decoration name.
    fn set_decoration(&mut self, big: bool, name: Option<String>) {
        if big {
            self.decorations.big = name;
        } else {
            self.decorations.small = name;
        }
    }

    /// Repair the personalities: an entry that does not name a duck is
    /// dropped with a clear message, and the numbers are clamped.
    ///
    /// WHY the repairs are recorded: the clamps were corrected in memory
    /// on every load and never written back — the warning repeated
    /// forever and the file kept the broken value (audit 2026-10-21 §E4).
    ///
    /// A **bad slot** is different, and its warning repeating on every
    /// load is the deliberate price of the fix (audit 2026-10-23 §F-056):
    /// the drop is memory-only and the entry stays in the file, because
    /// rewriting the `[[personality]]` array would eat the user's
    /// comments — so the load warns once per load until the user fixes
    /// the slot name themselves. One line per load, never a rewrite.
    fn repair_personalities(&mut self, repairs: &mut Vec<Repair>) {
        let listed = std::mem::take(&mut self.personality);
        let mut kept: Vec<PersonalityConfig> = Vec::with_capacity(listed.len());
        for (entry, mut p) in listed.into_iter().enumerate() {
            if p.duck_slot().is_none() {
                crate::trace::warn(&format!(
                    "config personality: slot {:?} does not name a duck, dropped",
                    p.slot
                ));
                continue;
            }
            if !(p.bob.is_finite() && p.bob >= 0.0) {
                crate::trace::warn(&format!(
                    "config personality {}: bob {} invalid, using 1.0",
                    p.slot, p.bob
                ));
                p.bob = 1.0;
                record_personality_float(repairs, entry, "bob", p.bob as f64);
            }
            if !(p.gait_speed.is_finite() && p.gait_speed > 0.0) {
                crate::trace::warn(&format!(
                    "config personality {}: gait_speed {} invalid, using 1.0",
                    p.slot, p.gait_speed
                ));
                p.gait_speed = 1.0;
                record_personality_float(repairs, entry, "gait_speed", p.gait_speed as f64);
            }
            if !(p.hop.is_finite() && p.hop >= 0.0) {
                crate::trace::warn(&format!(
                    "config personality {}: hop {} invalid, using 0.0",
                    p.slot, p.hop
                ));
                p.hop = 0.0;
                record_personality_float(repairs, entry, "hop", p.hop as f64);
            }
            kept.push(p);
        }
        self.personality = kept;
    }

    /// Grid dimensions derived from a monitor size in physical pixels.
    ///
    /// The cell comes from a row budget, not from `cell_size`:
    /// `cell_h = monitor_h / 360`, `cell_w = cell_h * 2/3` — so a 70-row big
    /// duck is ~19% of screen height on every monitor and every DPI level
    /// (audit 2026-10-06 §4.5). The PerMonitorV2 manifest reports true
    /// physical pixels, so the DPI `scale` folds in by itself.
    ///
    /// **The cell is never smaller than 2 × 3.** The duck art is X-art
    /// strokes, and the font rasterizer drops a glyph that covers less
    /// than its cell: an `X` has **no** coverage at all in a cell that
    /// is one pixel wide, so a 1 × 2 cell (a 720p panel, a 768p laptop,
    /// a small browser window) renders the parade as empty space with a
    /// balloon in it. Two pixels wide is the smallest the art can be
    /// drawn in, and the row budget's own 2 × 3 cell is what a 1080p
    /// panel gets — so the floor keeps the art's proportions and the
    /// 1080p grid (and the byte-identical selftest gate) exactly as
    /// they were.
    ///
    /// The trade is deliberate: a short window gets **fewer rows**
    /// (208 instead of 624 on a 624-px window) — the same pixel
    /// budget, a coarser grid, and a legible duck.
    ///
    /// `scale` is therefore *not* used: the leading underscore makes the
    /// compiler reject any use of it, and `grid_ignores_the_scale_argument`
    /// pins the behaviour with a test (audit 2026-10-17 §Maintainability
    /// "sanitize lies"). The parameter is kept only so the existing call
    /// sites — popup, CLI, selftest, audit prototype — keep compiling;
    /// removing it would ripple across four files.
    /// Returns `(cols, rows, cell_w, cell_h)`.
    pub fn grid_for_monitor(
        &self,
        monitor_w: u32,
        monitor_h: u32,
        _scale: f64,
    ) -> (usize, usize, u32, u32) {
        let cell_h = (monitor_h / 360).max(3);
        let cell_w = (((cell_h as f64) * 2.0 / 3.0).round() as u32).max(2);
        let cols = (monitor_w as usize / cell_w as usize).max(1);
        let cols = cols.min(self.max_cols.max(bounds::MAX_COLS_FLOOR));
        let rows = (monitor_h as usize / cell_h as usize).max(1);
        (cols, rows, cell_w, cell_h)
    }

    /// The personality of one train slot, if it has one.
    ///
    /// The lookup goes through [`PersonalityConfig::duck_slot`], so the
    /// user's numbering (`small_3`) and the parade's slot numbering can
    /// never drift apart.
    pub fn personality_for(&self, train_slot: usize) -> Option<&PersonalityConfig> {
        self.personality
            .iter()
            .find(|p| p.duck_slot() == Some(train_slot))
    }
}

/// Upper-case a message and drop the characters the block font cannot draw.
///
/// Returns the cleaned text and the dropped characters, so the repair can
/// report every one of them.
fn clean_message(raw: &str) -> (String, Vec<char>) {
    let mut out = String::with_capacity(raw.len());
    let mut dropped = Vec::new();
    for ch in raw.chars() {
        if !crate::hud_font::supports(ch) {
            dropped.push(ch);
            continue;
        }
        out.push(ch.to_ascii_uppercase());
    }
    (out, dropped)
}

/// Drop the characters the block font cannot draw, keeping the case.
///
/// The exit hint is drawn upper-case anyway, so the repair only drops —
/// it does not rewrite what the user wrote (audit 2026-10-22 §UI-19).
fn clean_hint(raw: &str) -> (String, Vec<char>) {
    let mut out = String::with_capacity(raw.len());
    let mut dropped = Vec::new();
    for ch in raw.chars() {
        if !crate::hud_font::supports(ch) {
            dropped.push(ch);
            continue;
        }
        out.push(ch);
    }
    (out, dropped)
}

/// Record a repaired float value.
fn record_float(repairs: &mut Vec<Repair>, section: &'static str, key: &'static str, value: f64) {
    repairs.push(Repair {
        section,
        key: key.to_string(),
        value: toml::Value::Float(value).to_string(),
    });
}

/// Record a repaired integer value.
fn record_int(repairs: &mut Vec<Repair>, section: &'static str, key: &'static str, value: i64) {
    repairs.push(Repair {
        section,
        key: key.to_string(),
        value: toml::Value::Integer(value).to_string(),
    });
}

/// Record a repaired string value.
fn record_string(repairs: &mut Vec<Repair>, section: &'static str, key: &'static str, value: &str) {
    repairs.push(Repair {
        section,
        key: key.to_string(),
        value: toml::Value::String(value.to_string()).to_string(),
    });
}

/// Record a repaired array value.
fn record_array(
    repairs: &mut Vec<Repair>,
    section: &'static str,
    key: &'static str,
    values: [u32; 2],
) {
    let value = toml::Value::Array(
        values
            .iter()
            .map(|v| toml::Value::Integer(*v as i64))
            .collect(),
    );
    repairs.push(Repair {
        section,
        key: key.to_string(),
        value: value.to_string(),
    });
}

/// Write the repaired values back into the config file.
///
/// The write is best effort: a config that cannot be written is still
/// perfectly usable in memory, so a failure is a warning, not an error.
/// The write replaces the repaired values **in place**, so the user's
/// comments and unknown keys survive (audit 2026-10-20 §3.10).
fn persist_repairs(path: &Path, repairs: &[Repair]) {
    if repairs.is_empty() {
        return;
    }
    if let Err(e) = crate::config_edit::write_repairs(path, repairs) {
        crate::trace::warn(&format!("config: could not write the repairs back: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawn::DrawnSize;

    /// The repair pass's fallback values are the `Default` values.
    ///
    /// WHY: the repair used to carry a **second copy** of every default
    /// as a literal, so a change to the default silently left the
    /// repair on the old number — and nothing compared the two (audit
    /// 2026-10-21 §E4).
    #[test]
    fn the_fallback_values_are_the_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.loop_seconds, bounds::LOOP_SECONDS_DEFAULT);
        assert_eq!(cfg.cell_size[0], bounds::CELL_WIDTH_DEFAULT);
        assert_eq!(cfg.cell_size[1], bounds::CELL_HEIGHT_DEFAULT);
        assert_eq!(cfg.max_cols, bounds::MAX_COLS_MIGRATED);
        assert_eq!(cfg.fps_cap, bounds::FPS_CAP_DEFAULT);
        assert_eq!(cfg.balloons.max, bounds::BALLOONS_MAX_MAX);
        assert_eq!(
            cfg.balloons.balloon_radius_rows,
            bounds::BALLOON_RADIUS_ROWS_DEFAULT
        );
        assert_eq!(cfg.balloons.rope_min_small, bounds::ROPE_MIN_SMALL_DEFAULT);
        assert_eq!(cfg.balloons.pendulum_g, bounds::PENDULUM_G_DEFAULT);
        assert_eq!(cfg.balloons.pendulum_damp, bounds::PENDULUM_DAMP_DEFAULT);
        assert_eq!(cfg.balloons.pendulum_drive, bounds::PENDULUM_DRIVE_DEFAULT);
        assert_eq!(
            cfg.balloons.pendulum_period,
            bounds::PENDULUM_PERIOD_DEFAULT
        );
        assert_eq!(
            cfg.message.fade_seconds,
            bounds::MESSAGE_FADE_SECONDS_DEFAULT
        );
        assert_eq!(cfg.audio.volume, bounds::AUDIO_VOLUME_DEFAULT);
        // The sky is on by default: the ramp is the shipped look
        // (audit 2026-10-23 §F-049).
        assert!(cfg.sky.enabled);
    }

    /// The default strip length is the crossing window at the default
    /// settings — a **frozen** number, pinned so a change to any of the
    /// three inputs is a conscious one (audit 2026-10-23 §F-048).
    ///
    /// 1145.0 is the train's length in cells at the built-in layout; the
    /// scene passes `parade.train_len()` there.
    #[test]
    fn the_default_strip_is_the_crossing_window() {
        let window = crate::audio::sequencer::crossing_window_steps(
            bounds::MAX_COLS_MIN,
            bounds::LOOP_SECONDS_DEFAULT,
            1145.0,
        );
        assert_eq!(window, 263, "the frozen default");
        assert_eq!(Config::default().audio.strip_steps, window);
    }

    /// `loop_seconds` is clamped at both ends by the named bounds.
    #[test]
    fn the_loop_seconds_is_clamped_into_the_bounds() {
        let cfg = Config {
            loop_seconds: 100_000.0,
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.loop_seconds, bounds::LOOP_SECONDS_MAX);
        let cfg = Config {
            loop_seconds: 0.0,
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.loop_seconds, bounds::LOOP_SECONDS_DEFAULT);
    }

    /// `pendulum_drive` is clamped by the named bounds.
    #[test]
    fn the_pendulum_drive_is_clamped_into_the_bounds() {
        let cfg = Config {
            balloons: BalloonsConfig {
                pendulum_drive: f32::NAN,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.balloons.pendulum_drive, bounds::PENDULUM_DRIVE_DEFAULT);
        let cfg = Config {
            balloons: BalloonsConfig {
                pendulum_drive: -1.0,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.balloons.pendulum_drive, bounds::PENDULUM_DRIVE_DEFAULT);
        let cfg = Config {
            balloons: BalloonsConfig {
                pendulum_drive: 99.0,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.balloons.pendulum_drive, bounds::PENDULUM_DRIVE_MAX);
    }

    /// A repaired personality is written back into the file.
    ///
    /// WHY: the clamps were corrected in memory on every load and never
    /// written back, so the warning repeated forever and the file kept
    /// the broken value (audit 2026-10-21 §E4).
    #[test]
    fn the_personality_repairs_are_written_back() {
        let dir = std::env::temp_dir().join("ducks-config-personality-repair");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[[personality]]
slot = \"big_1\"
bob = -1.0

[[personality]]
slot = \"small_1\"
bob = 2.0
",
        )
        .expect("write the sample");

        let cfg = Config::load_from(&path, assets_present).expect("the load succeeds");
        assert_eq!(cfg.personality.len(), 2, "both entries survive");
        assert_eq!(cfg.personality[0].bob, 1.0, "the broken bob is repaired");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("bob = 1.0"),
            "the repair is written back: {after}"
        );
        assert!(
            after.contains("bob = 2.0"),
            "the other entry is untouched: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The existence predicate for the tests that are **not** about the
    /// drawn assets.
    ///
    /// The repair pass drops an assignment whose asset is gone, so a
    /// test that measures something else must not have its subject
    /// dropped by accident. Naming every asset as present keeps the
    /// repair pass away from the value under test — and the tests that
    /// *are* about a missing asset inject their own predicate.
    fn assets_present(_kind: DrawnKind, _name: &str) -> bool {
        true
    }

    #[test]
    fn defaults_match_plan() {
        let c = Config::default();
        assert_eq!(c.loop_seconds, 20.0);
        assert_eq!(c.cell_size, [12, 24]);
        assert_eq!(c.max_cols, 4096);
        assert_eq!(c.fps_cap, 30);
        assert_eq!(c.monitor, MonitorChoice::Cursor);
        assert_eq!(c.hint_corner, HintCorner::BottomRight);
        assert_eq!(c.balloons.balloon_radius_rows, 36);
        assert_eq!(c.balloons.rope_min_small, 55);
        assert_eq!(c.balloons.rope_min_big, 85);
        assert_eq!(c.balloons.rope_max, 150);
        assert_eq!(c.balloons.pendulum_period, 1.6);
    }

    #[test]
    fn the_browser_restore_is_off_by_default() {
        // The joke: the browser stays hidden when the parade ends. The
        // user un-minimizes it themselves.
        let c = Config::default();
        assert!(!c.browsers.restore_on_close, "the restore must be opt-in");
        // A config file written before the key existed keeps the default.
        let cfg: Config = toml::from_str("[browsers]\nnames = [\"chrome\"]\n").expect("load");
        assert!(
            !cfg.browsers.restore_on_close,
            "an old config keeps the joke"
        );
        // The escape hatch: the user asks for the restore.
        let cfg: Config = toml::from_str("[browsers]\nrestore_on_close = true\n").expect("load");
        assert!(cfg.browsers.restore_on_close);
    }

    #[test]
    fn grid_1080p_is_960x360_row_budget() {
        let c = Config::default();
        let (cols, rows, cw, ch) = c.grid_for_monitor(1920, 1080, 1.0);
        assert_eq!((cols, rows, cw, ch), (960, 360, 2, 3));
        // Same grid at 4K/125%: the row budget folds DPI in by working in
        // physical pixels, so "70 rows ~= 20% of height" holds everywhere.
        let (cols, rows, cw, ch) = c.grid_for_monitor(3840, 2160, 1.25);
        assert_eq!((cols, rows, cw, ch), (960, 360, 4, 6));
    }

    #[test]
    fn grid_clamps_only_when_max_cols_says_so() {
        let mut c = Config::default();
        // 5120-wide ultrawide at cell 2: 2560 cols, under the 4096 cap.
        let (cols, _, _, _) = c.grid_for_monitor(5120, 1080, 1.0);
        assert_eq!(cols, 2560);
        // A lowered cap still clamps.
        c.max_cols = 300;
        let (cols, _, _, _) = c.grid_for_monitor(5120, 1080, 1.0);
        assert_eq!(cols, 300);
    }

    #[test]
    fn grid_never_goes_below_a_two_by_three_cell() {
        // The duck art is X-art strokes and the font rasterizer drops a
        // glyph that covers less than its cell: an 'X' has NO coverage
        // in a one-pixel-wide cell, so a 1 x 1 or a 1 x 2 cell renders
        // the parade as empty space with a balloon in it. The cell is
        // therefore never smaller than 2 x 3.
        let c = Config::default();
        // A short window: the row budget would ask for one row, the
        // floor gives three — and the same pixel budget buys a third
        // of the rows (a coarser grid, a legible duck).
        let (cols, rows, cw, ch) = c.grid_for_monitor(1280, 624, 1.0);
        assert_eq!((cols, rows, cw, ch), (640, 208, 2, 3));
        // Every panel below the 1080-row budget lands on the same floor.
        for height in [1_u32, 360, 480, 624, 720, 900, 1079] {
            let (_, _, cw, ch) = c.grid_for_monitor(1280, height, 1.0);
            assert_eq!((cw, ch), (2, 3), "a {height}-px-tall panel");
        }
        // The floor is a floor, not a cap: a 1080p panel still gets the
        // row budget's own 2 x 3 cell, and a 4K one gets 4 x 6.
        let (_, _, cw, ch) = c.grid_for_monitor(1920, 1080, 1.0);
        assert_eq!((cw, ch), (2, 3));
        let (_, _, cw, ch) = c.grid_for_monitor(3840, 2160, 1.25);
        assert_eq!((cw, ch), (4, 6));
    }

    /// The floor is not a number: the art has to be **visible** at it.
    ///
    /// This is the regression test for the missing ducks: the grid used
    /// to hand the renderer a one-pixel-wide cell, the 'X' of the duck
    /// art had no coverage in it, and the parade rendered as an empty
    /// sky with a balloon in it. The frame below is rendered through the
    /// very path the app renders through (the core's own blitter), and
    /// the duck's colour has to be on it.
    #[test]
    fn the_floor_cell_still_draws_the_ducks() {
        let cfg = Config::default();
        let (cols, rows, cw, ch) = cfg.grid_for_monitor(1280, 720, 1.0);
        assert_eq!((cw, ch), (2, 3));
        let mut font = crate::font::Font::new(cw, ch).expect("the font opens");
        let mut scene = crate::scene::Scene::new_with_assets(
            &cfg,
            cols,
            rows,
            cw,
            ch,
            &crate::assets::AssetDirs::default(),
        );
        scene.advance(1.0);
        let mut fb = crate::blit::FrameBuffer::new(cols as u32 * cw, rows as u32 * ch);
        crate::selftest::blit_scene_into(&scene, &mut font, &mut fb);
        let ducks = fb
            .data
            .chunks_exact(4)
            .filter(|pixel| pixel[0] == 255 && pixel[1] == 220 && pixel[2] == 80)
            .count();
        assert!(
            ducks > 500,
            "the floor cell must draw the ducks, got {ducks} duck pixels"
        );
    }

    #[test]
    fn grid_ignores_the_scale_argument() {
        // The row budget works in physical pixels, so the DPI scale is not an
        // input. The parameter survives only for call-site compatibility;
        // this test makes the non-use explicit (audit §Maintainability).
        let c = Config::default();
        let base = c.grid_for_monitor(1920, 1080, 1.0);
        for scale in [0.5, 1.0, 1.25, 2.0, 4.0] {
            assert_eq!(
                c.grid_for_monitor(1920, 1080, scale),
                base,
                "scale {scale} changed the grid"
            );
        }
    }

    #[test]
    fn parse_config_toml_roundtrip() {
        let txt = r#"
loop_seconds = 12.0
cell_size = [10, 20]
monitor = "primary"
max_cols = 200
fps_cap = 60
exit_hint = "esc!"
hint_corner = "bottom-left"

[balloons]
max = 5
balloon_radius_rows = 30
rope_min_small = 35
rope_min_big = 70
rope_max = 140

[hotkey]
windows = "A+G+Shift+Space"

[browsers]
names = ["chrome"]
"#;
        let cfg: Config = toml::from_str(txt).unwrap();
        assert_eq!(cfg.loop_seconds, 12.0);
        assert_eq!(cfg.monitor, MonitorChoice::Primary);
        assert_eq!(cfg.hint_corner, HintCorner::BottomLeft);
        assert_eq!(cfg.balloons.balloon_radius_rows, 30);
        assert_eq!(cfg.balloons.rope_min_small, 35);
        assert_eq!(cfg.balloons.rope_min_big, 70);
        assert_eq!(cfg.balloons.rope_max, 140);
        // Unspecified keys keep defaults.
        assert_eq!(cfg.max_cols, 200);
        assert_eq!(cfg.balloons.pendulum_g, 9.8);
        assert_eq!(cfg.hotkey.linux, "A+G+Shift+Space");
    }

    #[test]
    fn pre_fix2_config_keys_are_ignored() {
        // Old configs carrying diameter_scale/heights/rope_length still load;
        // the stale keys are dropped and the new semantics default in.
        let txt = r#"
[balloons]
diameter_scale = 1.2
heights = [3, 8, 13]
rope_length = 7.0
"#;
        let cfg: Config = toml::from_str(txt).unwrap();
        let cfg = cfg.sanitized(assets_present);
        assert_eq!(cfg.balloons.balloon_radius_rows, 36);
        assert_eq!(cfg.balloons.rope_min_small, 55);
        assert_eq!(cfg.balloons.rope_min_big, 85);
        assert_eq!(cfg.balloons.rope_max, 150);
    }

    #[test]
    fn sanitized_repairs_zero_values() {
        let c = Config {
            loop_seconds: 0.0,
            cell_size: [0, 0],
            fps_cap: 0,
            balloons: BalloonsConfig {
                max: 0,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        };
        let c = c.sanitized(assets_present);
        assert_eq!(c.loop_seconds, 20.0);
        assert_eq!(c.cell_size, [12, 24]);
        assert_eq!(c.fps_cap, 30);
        assert_eq!(c.balloons.max, 5);
    }

    #[test]
    fn sanitized_clamps_the_sanitize_lies() {
        // `max_cols`: the row budget needs ~960 cols, and the cap is bounded.
        let c = Config {
            max_cols: 260,
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(c.max_cols, 4096);
        let c = Config {
            max_cols: 99_999,
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(c.max_cols, 8192);
        // The Scene holds 5 ring-buffer slots: the config must not promise more.
        let c = Config {
            balloons: BalloonsConfig {
                max: 64,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(c.balloons.max, 5);
        // Conversion is bounded before it starts.
        let c = Config {
            balloons: BalloonsConfig {
                balloon_radius_rows: 4096,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(c.balloons.balloon_radius_rows, 128);
        // The rope default agrees with the zero-repair value.
        let c = Config {
            balloons: BalloonsConfig {
                rope_min_small: 0,
                ..BalloonsConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(c.balloons.rope_min_small, 55);
    }

    #[test]
    fn browser_list_normalizes() {
        let mut b = BrowsersConfig::default();
        assert!(b.effective().contains(&"chrome".to_string()));
        b.names = vec!["MyBrowser.EXE".to_string(), "  ".to_string()];
        assert_eq!(b.effective(), vec!["mybrowser".to_string()]);
    }

    #[test]
    fn monitor_choice_parse() {
        assert_eq!(MonitorChoice::parse("Cursor"), Some(MonitorChoice::Cursor));
        assert_eq!(
            MonitorChoice::parse(" primary "),
            Some(MonitorChoice::Primary)
        );
        assert_eq!(MonitorChoice::parse("nope"), None);
    }

    #[test]
    fn editor_chord_defaults_to_ctrl_shift_d() {
        let cfg = Config::default();
        // Ctrl keeps the chord out of the typing path: a bare `Shift+D`
        // fired while the user typed a capital-D name in the parade's
        // input line (audit 2026-10-22 §UX-11).
        assert_eq!(cfg.hotkey.editor_spec(), "Ctrl+Shift+D");
    }

    #[test]
    fn config_without_editor_key_keeps_the_default() {
        // A config.toml written before the editor chord existed must keep
        // loading, and the effective editor chord stays the default.
        let txt = r#"
[hotkey]
windows = "A+G+Shift+Space"
linux = "A+G+Shift+Space"
macos = "Ctrl+Alt+Shift+Space"
"#;
        let cfg: Config = toml::from_str(txt).expect("an old config must still load");
        assert_eq!(cfg.hotkey.editor_spec(), "Ctrl+Shift+D");
        // The parade chord of that old config is untouched.
        assert_eq!(cfg.hotkey.windows, "A+G+Shift+Space");
    }

    #[test]
    fn editor_chord_can_be_overridden_and_disabled() {
        let txt = r#"
[hotkey]
editor = "Ctrl+Shift+E"
"#;
        let cfg: Config = toml::from_str(txt).expect("an explicit editor chord must load");
        assert_eq!(cfg.hotkey.editor_spec(), "Ctrl+Shift+E");
        // An empty editor spec disables the chord (the daemon skips it).
        let mut cfg = Config::default();
        cfg.hotkey.editor = String::new();
        assert!(cfg.hotkey.editor_spec().trim().is_empty());
    }

    #[test]
    fn an_unsupported_character_is_reported_and_dropped() {
        // The block font carries only A-Z 0-9 . : _ - / \ and space.
        let mut cfg = Config {
            message: MessageConfig {
                text: "HI! 42".to_string(),
                ..MessageConfig::default()
            },
            ..Config::default()
        };
        cfg = cfg.sanitized(assets_present);
        assert_eq!(cfg.message.text, "HI 42", "the bad char is dropped");
        // The rest of the message survives.
        assert!(cfg.message.text.contains("42"));
    }

    #[test]
    fn the_message_is_upper_cased() {
        let cfg = Config {
            message: MessageConfig {
                text: "bring your own ducks".to_string(),
                ..MessageConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.message.text, "BRING YOUR OWN DUCKS");
    }

    #[test]
    fn the_exit_hint_drops_what_the_font_cannot_draw() {
        // The block font carries no parentheses: a natural hint used to
        // lose them silently on screen (audit 2026-10-22 §UI-19).
        let cfg = Config {
            exit_hint: "press ESC (or F12) to quit".to_string(),
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(
            cfg.exit_hint, "press ESC or F12 to quit",
            "the unsupported characters are gone, the words stay"
        );
    }

    #[test]
    fn the_default_exit_hint_is_not_repaired() {
        let mut repairs: Vec<Repair> = Vec::new();
        let cfg = Config::default().sanitized_with_reporting(assets_present, &mut repairs);
        assert_eq!(
            cfg.exit_hint, "press ESC to exit",
            "the default passes through untouched"
        );
        assert!(
            !repairs.iter().any(|repair| repair.key == "exit_hint"),
            "a default config fires no exit-hint repair: {repairs:?}"
        );
    }

    /// A blank exit hint is the one config value that traps the user: a
    /// fullscreen window with no on-screen exit. The repair falls back to
    /// the default hint and is **recorded**, so the file stops carrying a
    /// blank (audit 2026-10-23 §F-065).
    #[test]
    fn a_blank_exit_hint_falls_back_to_the_default() {
        let mut repairs: Vec<Repair> = Vec::new();
        let cfg = Config {
            exit_hint: "   ".to_string(),
            ..Config::default()
        }
        .sanitized_with_reporting(assets_present, &mut repairs);
        assert_eq!(cfg.exit_hint, DEFAULT_EXIT_HINT);
        assert!(
            repairs
                .iter()
                .any(|repair| repair.key == "exit_hint" && repair.value.contains("ESC")),
            "the fallback is recorded so it is written back: {repairs:?}"
        );
        // A hint made only of characters the font cannot draw is blank
        // **on screen**, so it falls back too.
        let cfg = Config {
            exit_hint: "!!!".to_string(),
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.exit_hint, DEFAULT_EXIT_HINT);
    }

    /// A grass tile or a decoration whose asset is missing is a **soft**
    /// drop: the name survives in the user's file, so drawing the asset
    /// later restores the override — including through the read-only
    /// verbs, which never rewrite the file for a soft repair
    /// (audit 2026-10-22 §P2-18 / 2026-10-23 §F-044).
    #[test]
    fn a_soft_repair_never_erases_the_name_from_the_file() {
        let dir = std::env::temp_dir().join("ducks-config-soft-repair");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my ducks\n\n[grass]\ntile = \"meadow\" # drawn tomorrow\n\n[decorations]\nbig = \"crown\"\n",
        )
        .expect("write the sample");

        // Nothing exists: both names are soft-dropped.
        let cfg = Config::load_from(&path, |_, _| false).expect("the load succeeds");
        assert_eq!(
            cfg.grass.tile, None,
            "the missing tile is dropped in memory"
        );
        assert_eq!(cfg.decorations.big, None);

        // The file keeps both names and the comment — nothing was erased.
        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("tile = \"meadow\" # drawn tomorrow"),
            "the pending grass name and its comment survive: {after}"
        );
        assert!(
            after.contains("big = \"crown\""),
            "the pending decoration name survives: {after}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sky_time_of_day_is_clamped() {
        let cfg = Config {
            sky: SkyConfig {
                enabled: true,
                time_of_day: 5.0,
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.sky.time_of_day, 1.0);
        let cfg = Config {
            sky: SkyConfig {
                enabled: true,
                time_of_day: f32::NAN,
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.sky.time_of_day, 0.35);
    }

    #[test]
    fn the_weather_intensity_is_clamped() {
        let cfg = Config {
            weather: WeatherConfig {
                mode: WeatherMode::Rain,
                intensity: 9.0,
                seed: 1,
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.weather.intensity, 2.0);
    }

    #[test]
    fn a_weather_mode_is_read_from_the_config() {
        let cfg: Config = toml::from_str("[weather]\nmode = \"snow\"\nseed = 7\n").expect("load");
        assert_eq!(cfg.weather.mode, WeatherMode::Snow);
        assert_eq!(cfg.weather.seed, 7);
        // A config without the table keeps the defaults.
        let cfg: Config = toml::from_str("loop_seconds = 12.0").expect("load");
        assert_eq!(cfg.weather.mode, WeatherMode::Off);
    }

    #[test]
    fn an_empty_personality_list_is_not_written() {
        // An inline `personality = []` cannot live next to the
        // `[[personality]]` tables the user writes, so the key is
        // omitted when the list is empty.
        let text = toml::to_string_pretty(&Config::default()).expect("serialize");
        assert!(
            !text.contains("personality"),
            "the empty list must be omitted: {text}"
        );
    }

    #[test]
    fn a_personality_is_read_from_the_config() {
        let cfg: Config =
            toml::from_str("[[personality]]\nslot = \"small_3\"\nbob = 1.5\ngait_speed = 1.2\n")
                .expect("load");
        assert_eq!(cfg.personality.len(), 1);
        assert_eq!(cfg.personality[0].slot, "small_3");
        assert_eq!(cfg.personality[0].bob, 1.5);
        assert_eq!(cfg.personality[0].gait_speed, 1.2);
        // The default is 1.0 for the numbers the user did not set.
        assert_eq!(cfg.personality[0].phase_offset, 0);
    }

    #[test]
    fn assignments_default_to_none() {
        let a = AssignmentsConfig::default();
        assert_eq!(a, AssignmentsConfig::default());
        for number in 1..=2 {
            assert_eq!(a.big(number), None, "big {number}");
        }
        for number in 1..=9 {
            assert_eq!(a.small(number), None, "small {number}");
        }
        for number in 1..=5 {
            assert_eq!(a.balloon(number), None, "balloon {number}");
        }
        // Out-of-range numbers read as "no assignment", never as a panic.
        assert_eq!(a.big(0), None);
        assert_eq!(a.big(3), None);
        assert_eq!(a.small(10), None);
        assert_eq!(a.balloon(6), None);
        // A config without an `[assignments]` table still loads.
        let cfg: Config = toml::from_str("loop_seconds = 12.0").expect("load");
        assert_eq!(cfg.assignments, AssignmentsConfig::default());
    }

    #[test]
    fn assignments_drop_empty_names() {
        let mut a = AssignmentsConfig {
            small_1: Some("   ".to_string()),
            big_1: Some("cat".to_string()),
            balloon_2: Some(String::new()),
            ..AssignmentsConfig::default()
        };
        // The existence predicate is injected: the test never touches the
        // user's drawn folders.
        a.sanitized_with(|_, _| true);
        assert_eq!(a.small_1, None, "a blank name is dropped");
        assert_eq!(a.balloon_2, None, "an empty name is dropped");
        assert_eq!(a.big_1, Some("cat".to_string()), "a valid name survives");
        // A name that no drawn asset carries is dropped as well (soft
        // repair: the file may appear later).
        let mut b = AssignmentsConfig {
            small_9: Some("bird".to_string()),
            ..AssignmentsConfig::default()
        };
        b.sanitized_with(|_, _| false);
        assert_eq!(b.small_9, None);
        // The name is trimmed before it is stored.
        let mut c = AssignmentsConfig {
            big_2: Some("  dog  ".to_string()),
            ..AssignmentsConfig::default()
        };
        c.sanitized_with(|_, _| true);
        assert_eq!(c.big_2, Some("dog".to_string()));
    }

    #[test]
    fn the_decorations_default_to_bare_ducks() {
        let c = Config::default();
        assert_eq!(c.decorations.big, None);
        assert_eq!(c.decorations.small, None);
        assert_eq!(c.decorations.get(DrawnSize::Big), None);
        // A config file written before the table existed keeps the
        // default.
        let cfg: Config = toml::from_str("loop_seconds = 12.0\n").expect("load");
        assert_eq!(cfg.decorations.big, None);
    }

    #[test]
    fn the_decorations_are_read_from_the_config() {
        let cfg: Config =
            toml::from_str("[decorations]\nbig = \"crown\"\nsmall = \"bowtie\"\n").expect("load");
        assert_eq!(cfg.decorations.big.as_deref(), Some("crown"));
        assert_eq!(cfg.decorations.small.as_deref(), Some("bowtie"));
        assert_eq!(cfg.decorations.get(DrawnSize::Big), Some("crown"));
        assert_eq!(cfg.decorations.get(DrawnSize::Small), Some("bowtie"));
    }

    #[test]
    fn a_decoration_that_does_not_exist_is_dropped() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        cfg.decorations.small = Some("  bowtie  ".to_string());
        let mut repairs: Vec<Repair> = Vec::new();
        // The existence predicate is injected: the test never touches the
        // user's drawn folders.
        let repaired = cfg.sanitized_with_reporting(
            |kind, name| kind == DrawnKind::Decoration && name == "crown",
            &mut repairs,
        );
        assert_eq!(
            repaired.decorations.big.as_deref(),
            Some("crown"),
            "a decoration that exists survives"
        );
        assert_eq!(
            repaired.decorations.small, None,
            "a decoration that does not exist is dropped"
        );
        // The drop is **memory-only**: no repair is recorded, so the
        // load writes nothing back and the file keeps the pending name —
        // draw the asset later and the override is still there
        // (audit 2026-10-22 §P2-18), the same soft policy the
        // assignment drop follows.
        assert!(
            !repairs.iter().any(|repair| repair.section == "decorations"),
            "a soft drop is not a repair to write back: {repairs:?}"
        );
    }

    #[test]
    fn an_empty_decoration_name_is_no_override() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("   ".to_string());
        let mut repairs: Vec<Repair> = Vec::new();
        let repaired = cfg.sanitized_with_reporting(|_, _| false, &mut repairs);
        assert_eq!(repaired.decorations.big, None, "a blank name is dropped");
        assert!(
            repairs.is_empty(),
            "an empty name is not a repair at all: {repairs:?}"
        );
    }

    #[test]
    fn a_missing_audio_table_keeps_the_defaults() {
        // A config file written before the table existed still loads.
        let cfg: Config = toml::from_str("loop_seconds = 12.0").expect("load");
        assert_eq!(cfg.audio, AudioConfig::default());
        assert_eq!(cfg.audio.volume, 0.7);
        assert!(!cfg.audio.muted);
        assert!(cfg.audio.track.is_none());
        assert_eq!(cfg.audio.strip_steps, 263);
        // A partial table keeps the other defaults.
        let cfg: Config = toml::from_str("[audio]\nmuted = true\n").expect("load");
        assert!(cfg.audio.muted);
        assert_eq!(cfg.audio.volume, 0.7);
        assert_eq!(cfg.audio.strip_steps, 263);
    }

    #[test]
    fn the_audio_volume_is_clamped() {
        let cfg = Config {
            audio: AudioConfig {
                volume: 5.0,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.volume, 1.0);
        let cfg = Config {
            audio: AudioConfig {
                volume: -1.0,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.volume, 0.0);
        // A volume that is not a number at all falls back to the default.
        let cfg = Config {
            audio: AudioConfig {
                volume: f32::NAN,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.volume, 0.7);
    }

    #[test]
    fn the_audio_strip_length_is_clamped() {
        let cfg = Config {
            audio: AudioConfig {
                strip_steps: 0,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.strip_steps, 1);
        let cfg = Config {
            audio: AudioConfig {
                strip_steps: 99_999,
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.strip_steps, 4096);
    }

    #[test]
    fn a_blank_audio_track_means_no_track() {
        let cfg = Config {
            audio: AudioConfig {
                track: Some(String::new()),
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert!(cfg.audio.track.is_none());
        let cfg = Config {
            audio: AudioConfig {
                track: Some("   ".to_string()),
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert!(cfg.audio.track.is_none());
        // A real name survives.
        let cfg = Config {
            audio: AudioConfig {
                track: Some("march".to_string()),
                ..AudioConfig::default()
            },
            ..Config::default()
        }
        .sanitized(assets_present);
        assert_eq!(cfg.audio.track.as_deref(), Some("march"));
    }

    #[test]
    fn the_audio_repairs_are_written_back() {
        let mut cfg = Config::default();
        cfg.audio.volume = 5.0;
        cfg.audio.strip_steps = 0;
        cfg.audio.track = Some(String::new());
        let mut repairs: Vec<Repair> = Vec::new();
        let repaired = cfg.sanitized_with_reporting(|_, _| true, &mut repairs);
        assert_eq!(repaired.audio.volume, 1.0);
        assert_eq!(repaired.audio.strip_steps, 1);
        assert!(repaired.audio.track.is_none());
        for key in ["volume", "strip_steps"] {
            assert!(
                repairs
                    .iter()
                    .any(|repair| repair.section == "audio" && repair.key == key),
                "the {key} repair is reported so it is written back: {repairs:?}"
            );
        }
        // A blank track name is "no track" — the same as an empty grass
        // tile or an empty decoration: the value is dropped **silently**,
        // with no warning and no repair, so the load does not report the
        // same thing on every run (audit 2026-10-21 §F10).
        assert!(
            !repairs
                .iter()
                .any(|repair| repair.section == "audio" && repair.key == "track"),
            "a blank track is not a repair: {repairs:?}"
        );
    }

    #[test]
    fn the_audio_table_round_trips_through_toml() {
        let cfg: Config = toml::from_str(
            "[audio]\nvolume = 0.3\nmuted = true\ntrack = \"march\"\nstrip_steps = 120\n",
        )
        .expect("load");
        assert_eq!(cfg.audio.volume, 0.3);
        assert!(cfg.audio.muted);
        assert_eq!(cfg.audio.track.as_deref(), Some("march"));
        assert_eq!(cfg.audio.strip_steps, 120);
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(text.contains("[audio]"), "the table is written: {text}");
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.audio, cfg.audio);
    }

    #[test]
    fn assignments_round_trip_through_toml() {
        let a = AssignmentsConfig {
            big_1: Some("cat".to_string()),
            small_9: Some("bird".to_string()),
            balloon_3: Some("party".to_string()),
            ..AssignmentsConfig::default()
        };
        let cfg = Config {
            assignments: a.clone(),
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).expect("serialize");
        assert!(
            text.contains("[assignments]"),
            "the table must be written: {text}"
        );
        assert!(
            text.contains("big_1 = \"cat\""),
            "the assignment must be written: {text}"
        );
        let back: Config = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.assignments, a, "the assignments must survive");
        assert_eq!(back.assignments.big(1), Some("cat"));
        assert_eq!(back.assignments.small(9), Some("bird"));
        assert_eq!(back.assignments.balloon(3), Some("party"));
        // The other keys keep their defaults.
        assert_eq!(back.assignments.big(2), None);
        assert_eq!(back.assignments.small(1), None);
    }

    #[test]
    fn assignment_slots_parse_in_both_spellings() {
        let slot = AssignmentSlot::parse("small_3").expect("small_3");
        assert_eq!(slot.kind, SlotKind::Small);
        assert_eq!(slot.number, 3);
        assert_eq!(slot.key(), "small_3");
        assert_eq!(AssignmentSlot::parse("SMALL3").expect("SMALL3"), slot);
        assert_eq!(AssignmentSlot::parse("small-3").expect("small-3"), slot);
        assert_eq!(AssignmentSlot::parse("Small3").expect("Small3"), slot);
        let big = AssignmentSlot::parse("BIG_2").expect("BIG_2");
        assert_eq!(big.kind, SlotKind::Big);
        assert_eq!(big.number, 2);
        let balloon = AssignmentSlot::parse("BALLOON5").expect("BALLOON5");
        assert_eq!(balloon.kind, SlotKind::Balloon);
        assert_eq!(balloon.number, 5);
        // Out-of-range and unknown names are refused.
        assert!(AssignmentSlot::parse("small_10").is_err());
        assert!(AssignmentSlot::parse("big_0").is_err());
        assert!(AssignmentSlot::parse("balloon_6").is_err());
        assert!(AssignmentSlot::parse("cat").is_err());
        assert!(AssignmentSlot::parse("").is_err());
    }

    #[test]
    fn assignment_slot_set_and_clear() {
        let mut a = AssignmentsConfig::default();
        let slot = AssignmentSlot::parse("small_3").expect("slot");
        a.set(slot, Some("cat".to_string()));
        assert_eq!(a.small(3), Some("cat"));
        a.set(slot, None);
        assert_eq!(a.small(3), None);
        // A slot that does not exist is ignored, not a panic.
        let ghost = AssignmentSlot {
            kind: SlotKind::Small,
            number: 42,
        };
        a.set(ghost, Some("cat".to_string()));
        assert_eq!(a.small(3), None);
    }
}

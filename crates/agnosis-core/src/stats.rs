//! The persistent counters (roadmap Extension 1 §1.7).
//!
//! A JSON file in the config dir, an atomic write, and a HUD element. The
//! stat file records the first parade's date, the number of parades, the
//! streak of consecutive days and the achievements earned.
//!
//! **The file is written atomically** (a temp file and a rename), so a
//! crash mid-write never leaves a broken file. A broken stat file is
//! treated as "no stats": the counters start over rather than the app
//! failing to start.
//!
//! The **achievements are listed in a file, not in the code** — the
//! definitions live in `<config>/achievements.toml`, so the user can add
//! one without a rebuild. The built-in set is the fallback when that file
//! is missing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The stat file's name inside the config dir.
pub const STATS_FILE: &str = "stats.json";

/// The achievement definitions' file name inside the config dir.
pub const ACHIEVEMENTS_FILE: &str = "achievements.toml";

/// The built-in achievements: few and silly.
pub const DEFAULT_ACHIEVEMENTS: [(&str, &str, u32, u32); 3] = [
    ("FIRST-PARADE", "YOUR FIRST PARADE", 1, 0),
    ("TEN-PARADES", "TEN PARADES", 10, 0),
    ("A-WEEK-IN-A-ROW", "A WEEK IN A ROW", 0, 7),
];

/// Failures produced by the stats file.
#[derive(Debug, thiserror::Error)]
pub enum StatsError {
    /// The file could not be written.
    #[error("could not write the stats file {path}: {source}")]
    Io {
        /// The file that could not be written.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The stats could not be rendered as JSON.
    #[error("could not render the stats: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// One achievement's definition.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AchievementDef {
    /// The machine name, stored in the stat file when it is earned.
    pub name: String,
    /// The title the user sees.
    pub title: String,
    /// The number of parades needed to earn it. `0` means "not a count".
    pub parades: u32,
    /// The number of consecutive days needed to earn it. `0` means "not a
    /// streak".
    pub streak: u32,
}

/// The counters the app keeps between runs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// The date of the first parade, as `YYYY-MM-DD`.
    pub first_run: String,
    /// The number of parades so far.
    pub parades: u32,
    /// The number of consecutive days with a parade.
    pub streak: u32,
    /// The date of the last parade, as `YYYY-MM-DD`.
    pub last_day: String,
    /// The achievements earned so far, by name.
    pub achievements: Vec<String>,
}

impl Stats {
    /// The counters of a fresh install.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Read the counters from `path`.
    ///
    /// A missing file, a broken file and a file of the wrong shape all
    /// read as "no stats": the counters start over rather than the app
    /// failing. The stat file is never on the UI thread's critical path
    /// more than once per parade.
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::empty();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            crate::trace::warn(&format!(
                "stats: {} is broken, starting over: {e}",
                path.display()
            ));
            Self::empty()
        })
    }

    /// Write the counters to `path`, atomically.
    ///
    /// The write goes to a temp file and is then renamed over the target,
    /// so a crash mid-write never leaves a broken file.
    pub fn save(&self, path: &Path) -> Result<(), StatsError> {
        let text = serde_json::to_string_pretty(self)?;
        write_atomic(path, &text)
    }

    /// Record a parade on `today` and return the achievements it earned.
    ///
    /// The streak grows when the last parade was yesterday, restarts when
    /// it was older, and is unchanged when it was today.
    pub fn record_parade(&mut self, today: &str, defs: &[AchievementDef]) -> Vec<String> {
        if self.first_run.is_empty() {
            self.first_run = today.to_string();
        }
        self.parades = self.parades.saturating_add(1);
        self.streak = next_streak(&self.last_day, today, self.streak);
        self.last_day = today.to_string();
        let earned = self.earn(defs);
        for name in &earned {
            if !self.achievements.contains(name) {
                self.achievements.push(name.clone());
            }
        }
        earned
    }

    /// The achievements the counters have earned but that are not recorded
    /// yet.
    fn earn(&self, defs: &[AchievementDef]) -> Vec<String> {
        defs.iter()
            .filter(|def| !def.name.is_empty())
            .filter(|def| !self.achievements.contains(&def.name))
            .filter(|def| self.earned_by(def))
            .map(|def| def.name.clone())
            .collect()
    }

    /// True when the counters satisfy the definition.
    fn earned_by(&self, def: &AchievementDef) -> bool {
        let by_count = def.parades > 0 && self.parades >= def.parades;
        let by_streak = def.streak > 0 && self.streak >= def.streak;
        by_count || by_streak
    }

    /// The number of days since the first parade.
    ///
    /// `0` before the first parade, and `0` when the first run's date
    /// cannot be read.
    pub fn days_since_first(&self, today: &str) -> u32 {
        days_between(&self.first_run, today).unwrap_or(0)
    }

    /// The number of parades so far.
    pub fn parades(&self) -> u32 {
        self.parades
    }

    /// The number of consecutive days with a parade.
    pub fn streak(&self) -> u32 {
        self.streak
    }

    /// The achievements earned so far.
    pub fn achievements(&self) -> &[String] {
        &self.achievements
    }
}

/// The streak after a parade on `today`.
fn next_streak(last_day: &str, today: &str, streak: u32) -> u32 {
    if last_day == today {
        return streak.max(1);
    }
    match days_between(last_day, today) {
        Some(1) => streak.saturating_add(1),
        Some(_) => 1,
        None => 1,
    }
}

/// The achievement definitions in `path`, or the built-in ones.
///
/// A missing file reads as the built-in set; a broken file does too (and
/// is traced), so a typo never costs the user their counters.
pub fn definitions(path: &Path) -> Vec<AchievementDef> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return default_definitions();
    };
    match parse_definitions(&text) {
        Ok(defs) if !defs.is_empty() => defs,
        Ok(_) => default_definitions(),
        Err(e) => {
            crate::trace::warn(&format!(
                "stats: {} is broken, using the built-in achievements: {e}",
                path.display()
            ));
            default_definitions()
        }
    }
}

/// The built-in achievement definitions.
pub fn default_definitions() -> Vec<AchievementDef> {
    DEFAULT_ACHIEVEMENTS
        .iter()
        .map(|(name, title, parades, streak)| AchievementDef {
            name: name.to_string(),
            title: title.to_string(),
            parades: *parades,
            streak: *streak,
        })
        .collect()
}

/// Parse the `[[achievement]]` tables of a definitions file.
fn parse_definitions(text: &str) -> Result<Vec<AchievementDef>, toml::de::Error> {
    #[derive(Deserialize)]
    struct File {
        #[serde(default)]
        achievement: Vec<AchievementDef>,
    }
    let file: File = toml::from_str(text)?;
    Ok(file.achievement)
}

/// Today's date as `YYYY-MM-DD`, in UTC.
///
/// **The core has no clock.** The standard library's `SystemTime::now`
/// **panics** on `wasm32-unknown-unknown` ("time not implemented on
/// this platform") and the release profile aborts on a panic, so the
/// clock belongs to the host: the shell reads it and hands the date in
/// (audit 2026-10-22 §P1-7).
///
/// The **calendar** stays here: the core owns the date arithmetic, the
/// host owns the clock. The host formats its reading with
/// [`date_from_unix_seconds`].
pub fn date_from_unix_seconds(seconds: i64) -> String {
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// The number of days from `from` to `to`, or `None` when a date cannot be
/// read.
pub fn days_between(from: &str, to: &str) -> Option<u32> {
    let start = days_from_civil(from)?;
    let end = days_from_civil(to)?;
    let span = end.checked_sub(start)?;
    Some(span.max(0) as u32)
}

/// Parse a `YYYY-MM-DD` date into the days since the Unix epoch.
///
/// The day is validated against the **month it claims**: `2026-02-30`
/// used to parse into a day in March (the civil arithmetic normalises
/// it silently), so an impossible date read as a real one
/// (audit 2026-10-23 §F-059). It is refused now.
fn days_from_civil(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    if day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil_parts(year, month, day))
}

/// The number of days in a month; `0` for a month that is not one.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Gregorian leap-year rule.
fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// The days since the Unix epoch of a civil date.
fn days_from_civil_parts(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a number of days since the Unix epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year, month as u32, day as u32)
}

/// Write `text` to `path` atomically: a temp file and a rename.
fn write_atomic(path: &Path, text: &str) -> Result<(), StatsError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StatsError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| STATS_FILE.to_string());
    let temp = path.with_file_name(format!("{name}.{}.tmp", std::process::id()));
    std::fs::write(&temp, text).map_err(|source| StatsError::Io {
        path: temp.clone(),
        source,
    })?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(StatsError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder that is not the user's real config folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir =
            std::env::temp_dir().join(format!("ducks-stats-{}-{nanos}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    #[test]
    fn a_missing_file_reads_as_no_stats() {
        let dir = temp_dir("missing");
        let stats = Stats::load(&dir.join(STATS_FILE));
        assert_eq!(stats, Stats::empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_reads_as_no_stats() {
        let dir = temp_dir("broken");
        let path = dir.join(STATS_FILE);
        std::fs::write(&path, "{ this is not json").expect("write");
        let stats = Stats::load(&path);
        assert_eq!(stats, Stats::empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_counters_survive_a_restart() {
        let dir = temp_dir("restart");
        let path = dir.join(STATS_FILE);
        let mut stats = Stats::empty();
        stats.record_parade("2026-10-21", &default_definitions());
        stats.save(&path).expect("save");
        let back = Stats::load(&path);
        assert_eq!(back.parades(), 1);
        assert_eq!(back.first_run, "2026-10-21");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_days_since_the_first_parade_are_counted() {
        let mut stats = Stats::empty();
        stats.record_parade("2026-10-01", &default_definitions());
        assert_eq!(stats.days_since_first("2026-10-01"), 0);
        assert_eq!(stats.days_since_first("2026-10-11"), 10);
        assert_eq!(stats.days_since_first("2026-11-01"), 31);
    }

    #[test]
    fn the_streak_grows_on_consecutive_days() {
        let defs = default_definitions();
        let mut stats = Stats::empty();
        stats.record_parade("2026-10-01", &defs);
        assert_eq!(stats.streak(), 1);
        stats.record_parade("2026-10-02", &defs);
        assert_eq!(stats.streak(), 2);
        stats.record_parade("2026-10-05", &defs);
        assert_eq!(stats.streak(), 1, "a gap restarts the streak");
    }

    #[test]
    fn a_second_parade_on_the_same_day_keeps_the_streak() {
        let defs = default_definitions();
        let mut stats = Stats::empty();
        stats.record_parade("2026-10-01", &defs);
        stats.record_parade("2026-10-01", &defs);
        assert_eq!(stats.streak(), 1);
        assert_eq!(stats.parades(), 2);
    }

    #[test]
    fn the_achievements_are_earned_once() {
        let defs = default_definitions();
        let mut stats = Stats::empty();
        let earned = stats.record_parade("2026-10-01", &defs);
        assert!(earned.contains(&"FIRST-PARADE".to_string()));
        let again = stats.record_parade("2026-10-02", &defs);
        assert!(
            !again.contains(&"FIRST-PARADE".to_string()),
            "an earned achievement is not earned twice"
        );
    }

    #[test]
    fn the_ten_parades_achievement_is_earned_on_the_tenth() {
        let defs = default_definitions();
        let mut stats = Stats::empty();
        for day in 1..=9 {
            stats.record_parade(&format!("2026-10-{day:02}"), &defs);
        }
        assert!(!stats.achievements().contains(&"TEN-PARADES".to_string()));
        let earned = stats.record_parade("2026-10-10", &defs);
        assert!(earned.contains(&"TEN-PARADES".to_string()));
    }

    #[test]
    fn a_week_in_a_row_is_earned_on_the_seventh_day() {
        let defs = default_definitions();
        let mut stats = Stats::empty();
        for day in 1..=6 {
            stats.record_parade(&format!("2026-10-{day:02}"), &defs);
        }
        assert!(!stats
            .achievements()
            .contains(&"A-WEEK-IN-A-ROW".to_string()));
        let earned = stats.record_parade("2026-10-07", &defs);
        assert!(earned.contains(&"A-WEEK-IN-A-ROW".to_string()));
    }

    #[test]
    fn the_definitions_can_live_in_a_file() {
        let dir = temp_dir("defs");
        let path = dir.join(ACHIEVEMENTS_FILE);
        std::fs::write(
            &path,
            "[[achievement]]\nname = \"MY-OWN\"\ntitle = \"MINE\"\nparades = 2\n",
        )
        .expect("write");
        let defs = definitions(&path);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "MY-OWN");
        let mut stats = Stats::empty();
        stats.record_parade("2026-10-01", &defs);
        let earned = stats.record_parade("2026-10-02", &defs);
        assert!(earned.contains(&"MY-OWN".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_definitions_file_falls_back_to_the_built_in_set() {
        let dir = temp_dir("broken-defs");
        let path = dir.join(ACHIEVEMENTS_FILE);
        std::fs::write(&path, "not = [ a table").expect("write");
        let defs = definitions(&path);
        assert_eq!(defs.len(), DEFAULT_ACHIEVEMENTS.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_date_arithmetic_survives_a_leap_year() {
        assert_eq!(days_between("2024-02-28", "2024-03-01"), Some(2));
        assert_eq!(days_between("2026-02-28", "2026-03-01"), Some(1));
        assert_eq!(days_between("2026-12-31", "2027-01-01"), Some(1));
        assert_eq!(days_between("2026-01-01", "2026-01-01"), Some(0));
    }

    #[test]
    fn a_broken_date_reads_as_no_span() {
        assert_eq!(days_between("not-a-date", "2026-01-01"), None);
        assert_eq!(days_between("2026-01-01", "not-a-date"), None);
    }

    /// An impossible date is refused, not silently normalised into the
    /// next month (audit 2026-10-23 §F-059).
    #[test]
    fn an_impossible_date_reads_as_no_span() {
        assert_eq!(days_between("2026-02-30", "2026-03-01"), None);
        assert_eq!(days_between("2026-04-31", "2026-05-01"), None);
        // February honours the leap-year rule.
        assert_eq!(days_between("2026-02-29", "2026-03-01"), None);
        assert_eq!(days_between("2024-02-29", "2024-03-01"), Some(1));
        assert_eq!(days_between("2000-02-29", "2000-03-01"), Some(1));
        assert_eq!(days_between("1900-02-29", "1900-03-01"), None);
        assert_eq!(days_between("2026-01-31", "2026-02-01"), Some(1));
    }

    #[test]
    fn a_date_is_a_well_formed_date() {
        // The core has no clock: the host reads it and hands the seconds
        // in (audit 2026-10-22 §P1-7).
        let text = date_from_unix_seconds(0);
        assert_eq!(text, "1970-01-01");
        let text = date_from_unix_seconds(1_700_000_000);
        assert_eq!(text.len(), 10, "the date must be YYYY-MM-DD: {text}");
        assert!(text.chars().nth(4) == Some('-'));
    }
}

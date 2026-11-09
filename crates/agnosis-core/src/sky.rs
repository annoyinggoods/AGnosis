//! The procedural sky gradient (roadmap Extension 1 §3.1a).
//!
//! The parade is drawn on black today. The sky is a **per-row colour
//! ramp** behind it: a zenith colour at the top of the frame, a horizon
//! colour at the grass line, and a dark earth below the grass.
//!
//! WHY a ramp and not a single colour: the weather composes with it — a
//! storm darkens the sky by lerping the ramp toward a storm colour, and
//! the lightning flash brightens it — so a single colour could not carry
//! both.
//!
//! **Determinism is mandatory.** The ramp is a pure function of
//! `time_of_day` and the row index: no clock, no randomness. The same
//! value always produces the same frame, which is what keeps the selftest
//! PNG gate byte-identical.

use crate::weather::Weather;

/// The zenith (top of the frame) and the horizon (the grass line) colours
/// at the five points of the day.
const MIDNIGHT: ([u8; 3], [u8; 3]) = ([6, 8, 26], [14, 16, 38]);
const DAWN: ([u8; 3], [u8; 3]) = ([34, 44, 96], [138, 78, 66]);
const DAY: ([u8; 3], [u8; 3]) = ([52, 102, 180], [104, 146, 188]);
const DUSK: ([u8; 3], [u8; 3]) = ([38, 32, 82], [128, 62, 56]);

/// The storm colour a rainy or snowy sky is lerped toward.
pub const STORM_COLOR: [u8; 3] = [48, 54, 72];

/// The colour the lightning flash lerps the sky toward.
pub const FLASH_COLOR: [u8; 3] = [255, 250, 220];

/// How far a storm darkens the sky.
///
/// The strength is deliberately modest: a storm must read as a storm, but
/// the **gradient** must survive it — a flat grey sky is not a sky.
/// Raised from 0.25 toward 0.35 (audit 2026-10-23 F-028): the old value
/// read as a slightly odd afternoon, and the rain needs a clearly darker
/// sky for its contrast tone to work against.
const STORM_STRENGTH: f32 = 0.35;

/// How far the lightning flash brightens the sky.
const FLASH_STRENGTH: f32 = 0.45;

/// How far into the night a time of day is, `0.0..=1.0` (audit
/// 2026-10-23 F-025).
///
/// `0.0` through the whole day half (dawn 0.25 to dusk 0.75 — no
/// starlight), rising to `1.0` at midnight. The night sky's stars and
/// moon scale their brightness by it, so they fade in through dusk
/// instead of popping on at a segment edge. A pure function of the value,
/// like everything in this module.
pub fn nightness(time_of_day: f32) -> f32 {
    let t = if time_of_day.is_finite() {
        time_of_day.clamp(0.0, 1.0)
    } else {
        0.5
    };
    // Depth into either night half, 0.25 per half, clamped at full night.
    let into_night = (0.25 - t).max(t - 0.75).max(0.0);
    (into_night / 0.25).min(1.0)
}

/// The sky's zenith and horizon colours at `time_of_day`.
///
/// `time_of_day` is `0.0..=1.0`, where `0.0` is midnight and `0.5` is
/// noon. The ramp runs night → dawn → day → dusk → night, so the two
/// ends of the range are the same colour.
pub fn sky_pair(time_of_day: f32) -> ([u8; 3], [u8; 3]) {
    let t = if time_of_day.is_finite() {
        time_of_day.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let (from, to, local) = segment(t);
    (
        lerp_colour(from.0, to.0, local),
        lerp_colour(from.1, to.1, local),
    )
}

/// A zenith/horizon colour pair.
type SkyPair = ([u8; 3], [u8; 3]);

/// The four segments of the day, and where `t` falls in them.
fn segment(t: f32) -> (SkyPair, SkyPair, f32) {
    if t < 0.25 {
        (MIDNIGHT, DAWN, t / 0.25)
    } else if t < 0.50 {
        (DAWN, DAY, (t - 0.25) / 0.25)
    } else if t < 0.75 {
        (DAY, DUSK, (t - 0.50) / 0.25)
    } else {
        (DUSK, MIDNIGHT, (t - 0.75) / 0.25)
    }
}

/// The colour of every grid row.
///
/// `ground_row` is the grass line: the sky fills the rows above it and
/// the earth fills the rows below. The result has one entry per row, so
/// the blitter can paint it in one pass.
pub fn ramp(time_of_day: f32, rows: usize, ground_row: i64) -> Vec<[u8; 3]> {
    let (zenith, horizon) = sky_pair(time_of_day);
    let earth = scale_colour(horizon, 0.14);
    let span = ground_row.max(1) as f32;
    (0..rows)
        .map(|row| {
            let row = row as i64;
            if row >= ground_row {
                return earth;
            }
            // Row 0 is the zenith, the ground row is the horizon.
            let depth = 1.0 - (row as f32 / span);
            lerp_colour(horizon, zenith, depth)
        })
        .collect()
}

/// Darken the ramp for a storm, or brighten it for a lightning flash.
///
/// The weather composes with the sky (roadmap Extension 1 §5.9): rain and
/// snow darken it, the lightning flash brightens it.
///
/// Only the **sky** rows are touched: the earth below the grass line
/// stays dark, or a flash would light up a fifth of the screen.
pub fn apply_weather(ramp: &mut [[u8; 3]], effect: Weather, flash: bool, ground_row: usize) {
    let sky = ground_row.min(ramp.len());
    if matches!(effect, Weather::Rain | Weather::Snow) {
        for colour in ramp[..sky].iter_mut() {
            *colour = lerp_colour(*colour, STORM_COLOR, STORM_STRENGTH);
        }
    }
    if flash {
        for colour in ramp[..sky].iter_mut() {
            *colour = lerp_colour(*colour, FLASH_COLOR, FLASH_STRENGTH);
        }
    }
}

/// The colour of one row of the ramp, or black when the sky is off.
///
/// The scene uses this to fade the custom message into its background.
pub fn row_colour(ramp: Option<&[[u8; 3]]>, row: usize) -> [u8; 3] {
    match ramp {
        Some(colours) => colours.get(row).copied().unwrap_or([0, 0, 0]),
        None => [0, 0, 0],
    }
}

/// Linear interpolation between two colours.
fn lerp_colour(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        let value = a as f32 + (b as f32 - a as f32) * t;
        value.round().clamp(0.0, 255.0) as u8
    };
    [
        mix(from[0], to[0]),
        mix(from[1], to[1]),
        mix(from[2], to[2]),
    ]
}

/// A colour scaled by `factor`.
fn scale_colour(colour: [u8; 3], factor: f32) -> [u8; 3] {
    let scale = |c: u8| -> u8 { ((c as f32 * factor).round().clamp(0.0, 255.0)) as u8 };
    [scale(colour[0]), scale(colour[1]), scale(colour[2])]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_has_one_colour_per_row() {
        let ramp = ramp(0.35, 40, 32);
        assert_eq!(ramp.len(), 40);
    }

    #[test]
    fn the_ramp_is_a_pure_function_of_the_time_of_day() {
        let first = ramp(0.42, 40, 32);
        let second = ramp(0.42, 40, 32);
        assert_eq!(first, second, "the same value must give the same frame");
    }

    #[test]
    fn the_zenith_is_above_the_horizon() {
        let ramp = ramp(0.5, 40, 32);
        let zenith = ramp[0];
        let horizon = ramp[31];
        assert!(
            horizon[2] > zenith[2],
            "the noon horizon must be lighter than the zenith: {horizon:?} vs {zenith:?}"
        );
    }

    #[test]
    fn the_earth_is_dark_below_the_grass() {
        let ramp = ramp(0.5, 40, 32);
        let sky = ramp[10];
        let earth = ramp[35];
        assert!(
            earth[0] < sky[0] && earth[1] < sky[1],
            "the earth must be darker than the sky: {earth:?} vs {sky:?}"
        );
    }

    #[test]
    fn midnight_and_noon_differ() {
        let night = sky_pair(0.0);
        let noon = sky_pair(0.5);
        assert_ne!(night, noon, "the two ends of the day must differ");
        // The ramp wraps: midnight and the end of the day are the same.
        assert_eq!(sky_pair(0.0), sky_pair(1.0));
    }

    #[test]
    fn a_storm_darkens_the_sky() {
        let mut ramp = ramp(0.5, 10, 8);
        let before = ramp[0];
        apply_weather(&mut ramp, Weather::Rain, false, 8);
        assert!(
            ramp[0][2] < before[2],
            "the rain must darken the sky: {:?} vs {:?}",
            ramp[0],
            before
        );
    }

    #[test]
    fn the_flash_brightens_the_sky() {
        let mut ramp = ramp(0.5, 10, 8);
        let before = ramp[0];
        apply_weather(&mut ramp, Weather::Lightning, true, 8);
        assert!(
            ramp[0][0] > before[0],
            "the flash must brighten the sky: {:?} vs {:?}",
            ramp[0],
            before
        );
    }

    #[test]
    fn the_weather_never_touches_the_earth() {
        // A flash that lit up the earth would light a fifth of the screen.
        let mut ramp = ramp(0.5, 10, 8);
        let earth = ramp[9];
        apply_weather(&mut ramp, Weather::Lightning, true, 8);
        assert_eq!(ramp[9], earth, "the earth must stay dark");
    }

    #[test]
    fn an_out_of_range_time_is_clamped() {
        assert_eq!(sky_pair(-1.0), sky_pair(0.0));
        assert_eq!(sky_pair(2.0), sky_pair(1.0));
        assert_eq!(sky_pair(f32::NAN), sky_pair(0.5));
    }

    #[test]
    fn the_nightness_covers_only_the_night_half() {
        // Full night at midnight, none through the day half, ramping
        // through dusk/dawn (audit 2026-10-23 F-025).
        assert_eq!(nightness(0.0), 1.0);
        assert_eq!(nightness(1.0), 1.0);
        assert_eq!(nightness(0.125), 0.5, "halfway into the night half");
        assert_eq!(nightness(0.25), 0.0, "dawn has no starlight");
        assert_eq!(nightness(0.5), 0.0, "noon has no starlight");
        assert_eq!(nightness(0.75), 0.0, "dusk has no starlight");
        assert_eq!(nightness(0.875), 0.5, "halfway into the evening");
        assert_eq!(nightness(f32::NAN), nightness(0.5));
    }
}

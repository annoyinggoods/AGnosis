//! The personality randomiser (roadmap HELP window §2, decision F-161).
//!
//! The settings panel's PERSONALITY row offers two actions:
//! **RANDOMISE** gives the eleven duck slots per-duck deltas within the
//! repair bounds, and **RESET** removes every `[[personality]]` table so
//! all ducks return to the built-in behaviour.
//!
//! This module is the **pure** half: a seed goes in, a personality list
//! comes out. The seed is a parameter — the core never reads the clock
//! (AGENTS.md: the host owns the clocks) — and the shell seeds it from
//! the host clock, so two presses of RANDOMISE never agree. The
//! personalities are written only into the user's `config.toml`, so the
//! selftest gate (which renders the built-in configuration) never sees
//! them.
//!
//! The ranges are **deltas**, not replacements: the parade reads as one
//! parade only while the ducks stay recognisably alike, so every value
//! is a nudge around the built-in behaviour that the repair pass
//! (`config.rs`'s `repair_personalities`) accepts unchanged — a value
//! the panel writes must survive the repair pass (AGENTS.md: the
//! bounds rule).

use crate::config::PersonalityConfig;

/// The randomiser's ranges, per field.
///
/// A tuple of `(min_inclusive, max_inclusive)`. Every range keeps the
/// generated values inside what the repair pass considers valid (finite,
/// `bob`/`hop >= 0`, `gait_speed > 0`) and inside what reads as a
/// *delta*: a gait half again as fast and a bob twice as deep are the
/// extremes.
const BOB_RANGE: (f32, f32) = (0.5, 2.0);
const GAIT_SPEED_RANGE: (f32, f32) = (0.8, 1.3);
/// The whole gait cycles a duck may lead or lag by. `GAIT_FPS` steps
/// are one cycle, so the offsets land the ducks' steps out of lockstep
/// without ever looking like a stutter.
const PHASE_RANGE: (i32, i32) = (0, crate::gait::GAIT_FPS as i32);
const HOP_RANGE: (f32, f32) = (0.0, 1.5);

/// A tiny deterministic PRNG (xorshift64\*), so the randomiser needs no
/// dependency and the same seed always draws the same personalities.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A float in `(min, max)`, from a fraction of the 64-bit state.
    fn float_in(&mut self, range: (f32, f32)) -> f32 {
        let fraction = (self.next_u64() >> 11) as f32 / (1u64 << 53) as f32;
        let (min, max) = range;
        min + (max - min) * fraction
    }

    /// An integer in `[min, max]`.
    fn int_in(&mut self, range: (i32, i32)) -> i32 {
        let span = (range.1 - range.0).max(0) as u64 + 1;
        range.0 + (self.next_u64() % span) as i32
    }
}

/// The eleven duck slot keys the randomiser covers, in parade order.
///
/// Balloons have no personality (`PersonalityConfig::duck_slot` answers
/// `None` for them), so they are not listed.
fn duck_slot_keys() -> Vec<String> {
    let kinds = [crate::config::SlotKind::Big, crate::config::SlotKind::Small];
    kinds
        .iter()
        .flat_map(|kind| {
            (1..=kind.count()).map(move |number| format!("{}_{}", kind.prefix(), number))
        })
        .collect()
}

/// Draw one personality for `slot`, deterministically from `rng`.
fn draw_personality(rng: &mut Rng, slot: String) -> PersonalityConfig {
    PersonalityConfig {
        slot,
        bob: round2(rng.float_in(BOB_RANGE)),
        gait_speed: round2(rng.float_in(GAIT_SPEED_RANGE)),
        phase_offset: rng.int_in(PHASE_RANGE),
        hop: round2(rng.float_in(HOP_RANGE)),
        // Colours stay the built-in yellow: the joke is the *behaviour*
        // deltas, and a parade of oddly tinted ducks reads as a bug.
        color: None,
    }
}

/// A float with two decimals, so the written TOML stays readable.
fn round2(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

/// Draw a personality for each of the eleven duck slots from `seed`.
///
/// Deterministic: the same seed draws the same set — which is what makes
/// the tests able to assert on the output, and what a user could use to
/// reproduce a favourite parade.
pub fn random_personalities(seed: u64) -> Vec<PersonalityConfig> {
    let mut rng = Rng(seed ^ 0xDA0C_5EED_1A55);
    duck_slot_keys()
        .into_iter()
        .map(|slot| draw_personality(&mut rng, slot))
        .collect()
}

/// The `[[personality]]` entries, rendered for the array-table splice.
///
/// An empty list renders an empty section: the splice then **removes**
/// the user's block — which is exactly what RESET is.
pub fn array_entries(personality: &[PersonalityConfig]) -> Vec<Vec<(String, Option<String>)>> {
    personality.iter().map(personality_entry).collect()
}

/// One `[[personality]]` entry, as rendered key-value pairs.
fn personality_entry(p: &PersonalityConfig) -> Vec<(String, Option<String>)> {
    let mut entry: Vec<(String, Option<String>)> = vec![(
        "slot".to_string(),
        Some(toml::Value::String(p.slot.clone()).to_string()),
    )];
    entry.push((
        "bob".to_string(),
        Some(toml::Value::Float(p.bob as f64).to_string()),
    ));
    entry.push((
        "gait_speed".to_string(),
        Some(toml::Value::Float(p.gait_speed as f64).to_string()),
    ));
    entry.push((
        "phase_offset".to_string(),
        Some(toml::Value::Integer(p.phase_offset as i64).to_string()),
    ));
    entry.push((
        "hop".to_string(),
        Some(toml::Value::Float(p.hop as f64).to_string()),
    ));
    if let Some(color) = p.color {
        entry.push(("color".to_string(), Some(color_value(color).to_string())));
    }
    entry
}

/// The TOML value of a personality's colour triple.
fn color_value(color: [u8; 3]) -> toml::Value {
    toml::Value::Array(
        color
            .iter()
            .map(|c| toml::Value::Integer(*c as i64))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The randomiser covers exactly the eleven duck slots.
    #[test]
    fn the_randomiser_covers_the_eleven_duck_slots() {
        let drawn = random_personalities(42);
        assert_eq!(drawn.len(), 11);
        assert_eq!(drawn[0].slot, "big_1");
        assert_eq!(drawn[1].slot, "big_2");
        assert_eq!(drawn[2].slot, "small_1");
        assert_eq!(drawn[10].slot, "small_9");
    }

    /// Every drawn slot names a duck the repair pass keeps.
    #[test]
    fn every_drawn_slot_names_a_duck() {
        for p in random_personalities(7) {
            assert!(p.duck_slot().is_some(), "{} must name a duck", p.slot);
        }
    }

    /// The same seed draws the same set — the randomiser is pure.
    #[test]
    fn the_same_seed_draws_the_same_set() {
        assert_eq!(random_personalities(9), random_personalities(9));
        assert_ne!(random_personalities(9), random_personalities(10));
    }

    /// Every drawn value is a delta the repair pass accepts unchanged.
    #[test]
    fn every_drawn_value_is_within_the_delta_ranges() {
        for p in random_personalities(3) {
            assert!(p.bob.is_finite() && p.bob >= BOB_RANGE.0 && p.bob <= BOB_RANGE.1);
            assert!(
                p.gait_speed.is_finite()
                    && p.gait_speed >= GAIT_SPEED_RANGE.0
                    && p.gait_speed <= GAIT_SPEED_RANGE.1
            );
            assert!((PHASE_RANGE.0..=PHASE_RANGE.1).contains(&p.phase_offset));
            assert!(p.hop.is_finite() && p.hop >= HOP_RANGE.0 && p.hop <= HOP_RANGE.1);
        }
    }

    /// An empty list renders an empty section: RESET's shape.
    #[test]
    fn an_empty_list_renders_no_entries() {
        assert!(array_entries(&[]).is_empty());
    }

    /// The rendered entries carry the keys the config parses back.
    #[test]
    fn the_entries_render_the_config_keys() {
        let drawn = random_personalities(5);
        let entries = array_entries(&drawn);
        let (slot, value) = &entries[0][0];
        assert_eq!(slot, "slot");
        assert_eq!(value.as_deref(), Some("\"big_1\""));
        let keys: Vec<&str> = entries[0].iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["slot", "bob", "gait_speed", "phase_offset", "hop"]);
    }
}

//! The one stale-stamp check the shell's loader workers share.
//!
//! Decision F-166 (roadmap HELP window §4.2): the three shell workers —
//! the asset loader, the backdrop loader and the typed-balloon loader —
//! genuinely differ in channel shape and generation semantics, so the
//! generic-helper refactor stays deferred. What they **do** share is the
//! staleness rule itself: a worker's answer carries the stamp of the
//! request it was made for (a generation number, a radii pair), and an
//! answer whose stamp differs from the live one is **stale** — dropped,
//! never applied. This module owns that one comparison, so the three
//! sites cannot drift apart in meaning; each keeps its own loop, channel
//! and drop behaviour.

/// Is a worker's answer still current?
///
/// `stamp` is what the answer was made for, `live` is what the caller
/// now works with. Equal means current; anything else is stale and the
/// caller drops the answer.
pub fn answer_is_current<T: PartialEq>(stamp: &T, live: &T) -> bool {
    stamp == live
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_current_when_its_stamp_matches() {
        assert!(answer_is_current(&7u64, &7u64));
        assert!(answer_is_current(&(36i64, 24i64), &(36, 24)));
    }

    #[test]
    fn an_answer_is_stale_when_its_stamp_differs() {
        assert!(!answer_is_current(&7u64, &8u64));
        assert!(!answer_is_current(&(36, 24), &(40, 26)));
    }
}

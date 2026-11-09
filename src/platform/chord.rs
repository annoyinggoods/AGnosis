//! Multi-key hotkey chord parsing and edge-triggered matching.
//!
//! Config chords are `A+G+Shift+Space` (Windows/Linux) and
//! `Ctrl+Alt+Shift+Space` (macOS). A chord is a set of required modifiers
//! plus an ordered list of non-modifier keys that must all be held together.
//!
//! Platform backends split the raw key stream in two before feeding it here:
//! transitions of Shift/Ctrl/Alt/Super go to
//! [`ChordMatcher::on_modifier`], while every other key transition goes to
//! [`ChordMatcher::on_key`] (`Key::Char` lowercased for letters,
//! `Key::Space` for the space bar). The matcher keeps the held-key set
//! itself and reports the trigger edge only.
//!
//! [`ChordSet`] holds several matchers at once — one per configured chord —
//! so the daemon can tell *which* chord fired: the parade hotkey opens the
//! parade, the editor hotkey opens the draw-your-own editor. Each matcher
//! keeps its own held-key set and its own latch, so the edge rules of a
//! single chord are unchanged; only the payload of a trigger differs (it
//! carries the [`HotkeyAction`] instead of nothing).
//!
//! ESC is never bound globally; only the popup window handles ESC. The combo
//! doubles as the popup toggle via cross-process close.

use std::collections::{BTreeSet, HashSet};

/// Modifier key in a chord spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Modifier {
    /// `Ctrl` / `Control`.
    Ctrl,
    /// `Alt` / `Option` / `Meta` (Meta is treated as Alt).
    Alt,
    /// `Shift`.
    Shift,
    /// `Super` / `Win` / `Windows` / `Cmd` / `Command`.
    Super,
}

/// Non-modifier key in a chord spec.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    /// Single character key, lowercased for ASCII letters.
    Char(char),
    /// Space bar.
    Space,
    /// `Esc` / `Escape`.
    Escape,
    /// Function key `F1`..=`F24`.
    F(u8),
    /// Any other key, carried exactly as the backend reported it.
    ///
    /// Not on macOS: the macOS `CGEventTap` backend maps the virtual key
    /// codes it knows (letters, digits, space, escape, `F1`–`F20`) and
    /// **drops** the rest, so it never needs the catch-all variant. Extra
    /// held keys never block a chord, so dropping them is harmless.
    #[cfg(not(target_os = "macos"))]
    Other(String),
}

/// Parsed chord: required modifiers plus ordered non-modifier keys.
#[derive(Debug, Clone, PartialEq)]
pub struct Chord {
    /// Modifiers that must all be held for the chord to fire.
    pub required_mods: BTreeSet<Modifier>,
    /// Non-modifier keys that must all be held, in spec order.
    pub keys: Vec<Key>,
}

/// What a chord fires.
///
/// The action travels with the trigger so the daemon knows *which* chord
/// was pressed without a second hotkey listener: `Popup` is the parade
/// hotkey, `Editor` is the draw-your-own hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    /// The parade hotkey: toggle the parade popup.
    Popup,
    /// The draw-your-own editor hotkey: toggle the editor window.
    Editor,
}

/// Failures from [`parse_chord`] and [`ChordMatcher::new`].
#[derive(Debug, thiserror::Error)]
pub enum ChordError {
    /// The spec was empty or only whitespace.
    #[error("chord spec is empty")]
    Empty,
    /// A `+`-separated segment was empty, e.g. `A++G` or a trailing `+`.
    #[error("chord spec {0:?} has an empty segment")]
    EmptySegment(String),
    /// A segment named no known modifier or key.
    #[error("chord spec has unknown token {0:?}")]
    UnknownToken(String),
    /// The chord binds `Esc`, which the **binding** path refuses: ESC is
    /// reserved for closing the popup and the editor windows and is never
    /// a global hotkey (audit F-134). The parser itself keeps the token
    /// legal — see [`ChordMatcher::binds_escape`].
    #[error("chord spec {0:?} binds Esc, which is reserved for closing the windows")]
    EscapeBound(String),
}

/// One parsed `+`-separated segment: a modifier or a non-modifier key.
enum Token {
    /// Modifier segment (`Ctrl`, `Shift`, ...).
    Mod(Modifier),
    /// Non-modifier key segment (`Space`, `A`, `F5`, ...).
    Key(Key),
}

/// Classify one lowercased chord segment (audit nesting site #17): keeps
/// [`parse_chord`] flat by moving the modifier/key/function-key ladder
/// into this single-decision helper.
fn classify_token(lower: &str, raw_token: &str) -> Result<Token, ChordError> {
    match lower {
        "ctrl" | "control" => Ok(Token::Mod(Modifier::Ctrl)),
        "alt" | "option" | "meta" => Ok(Token::Mod(Modifier::Alt)),
        "shift" => Ok(Token::Mod(Modifier::Shift)),
        "super" | "win" | "windows" | "cmd" | "command" => Ok(Token::Mod(Modifier::Super)),
        "space" => Ok(Token::Key(Key::Space)),
        "esc" | "escape" => Ok(Token::Key(Key::Escape)),
        _ => classify_key_token(lower, raw_token),
    }
}

/// Classify a non-modifier segment: function key, single char, or unknown.
fn classify_key_token(lower: &str, raw_token: &str) -> Result<Token, ChordError> {
    if let Some(n) = parse_function_key(lower) {
        return Ok(Token::Key(Key::F(n)));
    }
    if lower.chars().count() == 1 {
        let c = lower.chars().next().unwrap_or('?');
        return Ok(Token::Key(Key::Char(c)));
    }
    Err(ChordError::UnknownToken(raw_token.to_string()))
}

/// Parse a `+`-separated chord spec such as `A+G+Shift+Space`.
///
/// Segments are trimmed and matched case-insensitively. Modifiers are
/// `Ctrl`/`Control`, `Alt`/`Option`/`Meta`, `Shift`, and
/// `Super`/`Win`/`Windows`/`Cmd`/`Command`. Keys are `Space`,
/// `Esc`/`Escape`, `F1`..`F24`, or a single character (ASCII letters are
/// lowercased). A modifier-only spec parses successfully but its matcher
/// never fires.
pub fn parse_chord(spec: &str) -> Result<Chord, ChordError> {
    if spec.trim().is_empty() {
        return Err(ChordError::Empty);
    }
    let mut chord = Chord {
        required_mods: BTreeSet::new(),
        keys: Vec::new(),
    };
    for raw in spec.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return Err(ChordError::EmptySegment(spec.to_string()));
        }
        match classify_token(&token.to_ascii_lowercase(), token)? {
            Token::Mod(m) => {
                chord.required_mods.insert(m);
            }
            Token::Key(k) => chord.keys.push(k),
        }
    }
    Ok(chord)
}

fn parse_function_key(lower: &str) -> Option<u8> {
    let digits = lower.strip_prefix('f')?;
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: u8 = digits.parse().ok()?;
    if (1..=24).contains(&n) {
        Some(n)
    } else {
        None
    }
}

/// Edge-triggered matcher over a parsed [`Chord`].
///
/// Feed modifier transitions with [`ChordMatcher::on_modifier`] and all
/// other key transitions with [`ChordMatcher::on_key`]. `on_key` returns
/// true exactly once per press: when a chord key goes down while every other
/// required key and every required modifier is already held. The match then
/// latches and re-arms only after no chord key is held anymore, so key
/// repeat cannot re-fire. Extra held non-chord keys never block firing. A
/// modifier-only chord never fires.
///
/// Every platform drives the matcher: Windows and Linux through their
/// native hotkey backends, macOS through the listen-only `CGEventTap`
/// (roadmap Stage 3A).
#[derive(Debug)]
pub struct ChordMatcher {
    chord: Chord,
    held_mods: BTreeSet<Modifier>,
    held_keys: HashSet<Key>,
    fired: bool,
}

impl ChordMatcher {
    /// Build a matcher for `spec`, parsed with [`parse_chord`].
    pub fn new(spec: &str) -> Result<Self, ChordError> {
        Ok(Self {
            chord: parse_chord(spec)?,
            held_mods: BTreeSet::new(),
            held_keys: HashSet::new(),
            fired: false,
        })
    }

    /// Report a modifier-key transition.
    pub fn on_modifier(&mut self, m: Modifier, down: bool) {
        if down {
            self.held_mods.insert(m);
        } else {
            self.held_mods.remove(&m);
        }
    }

    /// Feed one non-modifier key transition; returns true on the trigger
    /// edge only. Key repeat (down while already held) never re-fires.
    ///
    /// The two directions are split into their own helpers (audit nesting
    /// rule) so neither decision sits inside the other's branch.
    pub fn on_key(&mut self, key: Key, down: bool) -> bool {
        if down {
            self.on_key_down(key)
        } else {
            self.on_key_up(key)
        }
    }

    /// Key-down half of [`ChordMatcher::on_key`].
    ///
    /// Fires the chord exactly on the edge where the last required key
    /// goes down while every required modifier is already held. A key
    /// that is not part of the chord, an already-fired matcher, and a
    /// modifier-only chord all return false without latching.
    fn on_key_down(&mut self, key: Key) -> bool {
        let is_chord_key = self.chord.keys.contains(&key);
        self.held_keys.insert(key);
        if !is_chord_key || self.fired || self.chord.keys.is_empty() {
            return false;
        }
        let mods_held = self
            .chord
            .required_mods
            .iter()
            .all(|m| self.held_mods.contains(m));
        let keys_held = self.chord.keys.iter().all(|k| self.held_keys.contains(k));
        if !mods_held || !keys_held {
            return false;
        }
        self.fired = true;
        true
    }

    /// Key-up half of [`ChordMatcher::on_key`].
    ///
    /// The matcher re-arms (`fired = false`) once the last chord key is
    /// released; releasing anything else leaves the latch alone.
    fn on_key_up(&mut self, key: Key) -> bool {
        self.held_keys.remove(&key);
        if !self.chord.keys.iter().any(|k| self.held_keys.contains(k)) {
            self.fired = false;
        }
        false
    }

    /// Does the chord's key list contain `Esc`?
    ///
    /// The **binding** path ([`ChordSet::new`], the one the global hotkey
    /// listener arms) refuses such a chord — ESC is never bound globally,
    /// only the popup and the editor windows handle it — while the parser
    /// keeps the token legal, so a window-local chord (and a test) can
    /// still spell it (audit F-134).
    pub fn binds_escape(&self) -> bool {
        self.chord.keys.contains(&Key::Escape)
    }

    /// Clear transient press state without changing the chord definition.
    /// Used to recover from a poisoned matcher lock (audit robustness).
    ///
    /// Windows-only: the Windows hotkey backend is the only one that shares
    /// the matcher behind a mutex, so it is the only one that can poison it.
    #[cfg(target_os = "windows")]
    pub fn clear_held(&mut self) {
        self.held_mods.clear();
        self.held_keys.clear();
        self.fired = false;
    }

    /// The parsed chord this matcher was built from.
    ///
    /// Test-only: production drives the matcher through
    /// [`ChordMatcher::on_modifier`] / [`ChordMatcher::on_key`] and never
    /// reads the chord back, so the accessor is kept behind `cfg(test)` for
    /// the parse assertions (audit dead-code sweep).
    #[cfg(test)]
    pub fn chord(&self) -> &Chord {
        &self.chord
    }
}

/// Several chords at once, each with the action it triggers.
///
/// The set is the multi-chord form of [`ChordMatcher`]: every configured
/// chord keeps its own matcher (and therefore its own held-key set and its
/// own latch), and a key-down is fed to *all* of them. The action of the
/// **first** matcher that fires wins, so one key press can never produce two
/// actions.
///
/// An empty spec string is skipped, not an error: that chord is disabled.
/// A spec that does not parse is an error, so the caller must decide
/// whether one broken chord may take the others down with it.
#[derive(Debug)]
pub struct ChordSet {
    entries: Vec<(ChordMatcher, HotkeyAction)>,
}

impl ChordSet {
    /// Build a set from `(spec, action)` pairs.
    ///
    /// Empty specs are skipped (the chord is disabled); the first spec that
    /// does not parse fails the whole set, and so does the first spec that
    /// binds `Esc` (audit F-134): ESC is reserved for closing the windows,
    /// so arming it globally must fail loudly instead of silently never
    /// firing.
    pub fn new(bindings: &[(String, HotkeyAction)]) -> Result<Self, ChordError> {
        let mut entries = Vec::with_capacity(bindings.len());
        for (spec, action) in bindings {
            if spec.trim().is_empty() {
                continue;
            }
            let matcher = ChordMatcher::new(spec)?;
            if matcher.binds_escape() {
                return Err(ChordError::EscapeBound(spec.clone()));
            }
            entries.push((matcher, *action));
        }
        Ok(Self { entries })
    }

    /// Report a modifier-key transition to every matcher.
    pub fn on_modifier(&mut self, m: Modifier, down: bool) {
        for (matcher, _) in &mut self.entries {
            matcher.on_modifier(m, down);
        }
    }

    /// Feed one non-modifier key transition to every matcher.
    ///
    /// Returns the action of the first matcher that fires on this
    /// transition, or `None` when no chord fired. A key repeat cannot fire
    /// twice: every matcher latches on its own until its chord keys are
    /// released.
    ///
    /// Every matcher owns its held-key set, so each one gets its own copy
    /// of the key; the copy is a cheap `String` clone only for the
    /// `Key::Other` fallback of an unmapped key.
    pub fn on_key(&mut self, key: Key, down: bool) -> Option<HotkeyAction> {
        let mut fired: Option<HotkeyAction> = None;
        for (matcher, action) in &mut self.entries {
            let hit = matcher.on_key(key.clone(), down);
            if hit && fired.is_none() {
                fired = Some(*action);
            }
        }
        fired
    }

    /// Clear transient press state of every matcher without changing the
    /// chord definitions.
    ///
    /// Windows-only: the Windows hotkey backend is the only one that shares
    /// the matchers behind a mutex, so it is the only one that can poison
    /// it.
    #[cfg(target_os = "windows")]
    pub fn clear_held(&mut self) {
        for (matcher, _) in &mut self.entries {
            matcher.clear_held();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(m: &[Modifier]) -> BTreeSet<Modifier> {
        m.iter().copied().collect()
    }

    #[test]
    fn parses_windows_chord() {
        let c = parse_chord("A+G+Shift+Space").unwrap();
        assert_eq!(c.required_mods, mods(&[Modifier::Shift]));
        assert_eq!(c.keys, vec![Key::Char('a'), Key::Char('g'), Key::Space]);
    }

    #[test]
    fn parses_macos_chord() {
        let c = parse_chord("Ctrl+Alt+Shift+Space").unwrap();
        assert_eq!(
            c.required_mods,
            mods(&[Modifier::Ctrl, Modifier::Alt, Modifier::Shift])
        );
        assert_eq!(c.keys, vec![Key::Space]);
    }

    #[test]
    fn accepts_synonyms() {
        let c = parse_chord("Control+Win+Esc").unwrap();
        assert_eq!(c.required_mods, mods(&[Modifier::Ctrl, Modifier::Super]));
        assert_eq!(c.keys, vec![Key::Escape]);
        let c = parse_chord("Option+Cmd+F5").unwrap();
        assert_eq!(c.required_mods, mods(&[Modifier::Alt, Modifier::Super]));
        assert_eq!(c.keys, vec![Key::F(5)]);
    }

    #[test]
    fn rejects_empty_spec() {
        assert!(matches!(parse_chord(""), Err(ChordError::Empty)));
        assert!(matches!(parse_chord("   "), Err(ChordError::Empty)));
    }

    #[test]
    fn rejects_empty_segments() {
        assert!(matches!(
            parse_chord("A++G"),
            Err(ChordError::EmptySegment(_))
        ));
        assert!(matches!(
            parse_chord("+A"),
            Err(ChordError::EmptySegment(_))
        ));
        assert!(matches!(
            parse_chord("A+"),
            Err(ChordError::EmptySegment(_))
        ));
    }

    #[test]
    fn rejects_unknown_tokens() {
        assert!(matches!(
            parse_chord("A+Enter"),
            Err(ChordError::UnknownToken(_))
        ));
        assert!(matches!(
            parse_chord("F0"),
            Err(ChordError::UnknownToken(_))
        ));
        assert!(matches!(
            parse_chord("F25"),
            Err(ChordError::UnknownToken(_))
        ));
    }

    #[test]
    fn parses_case_insensitively() {
        let upper = parse_chord("A+G+Shift+Space").unwrap();
        let lower = parse_chord("a+g+shift+space").unwrap();
        assert_eq!(upper, lower);
    }

    #[test]
    fn matcher_fires_on_last_key_down_edge() {
        let mut m = ChordMatcher::new("A+G+Shift+Space").unwrap();
        m.on_modifier(Modifier::Shift, true);
        assert!(!m.on_key(Key::Char('a'), true));
        assert!(!m.on_key(Key::Char('g'), true));
        assert!(m.on_key(Key::Space, true));
    }

    #[test]
    fn matcher_needs_modifiers_held() {
        let mut m = ChordMatcher::new("A+G+Shift+Space").unwrap();
        assert!(!m.on_key(Key::Char('a'), true));
        assert!(!m.on_key(Key::Char('g'), true));
        assert!(!m.on_key(Key::Space, true));
        assert!(!m.chord().required_mods.is_empty());
    }

    #[test]
    fn key_repeat_does_not_refire_until_release_all() {
        let mut m = ChordMatcher::new("A+G+Shift+Space").unwrap();
        m.on_modifier(Modifier::Shift, true);
        m.on_key(Key::Char('a'), true);
        m.on_key(Key::Char('g'), true);
        assert!(m.on_key(Key::Space, true));
        assert!(!m.on_key(Key::Space, true));
        assert!(!m.on_key(Key::Char('g'), true));
        m.on_key(Key::Space, false);
        assert!(!m.on_key(Key::Space, true));
        m.on_key(Key::Space, false);
        m.on_key(Key::Char('a'), false);
        m.on_key(Key::Char('g'), false);
        m.on_key(Key::Char('a'), true);
        m.on_key(Key::Char('g'), true);
        assert!(m.on_key(Key::Space, true));
    }

    #[test]
    fn extra_held_keys_do_not_block_firing() {
        let mut m = ChordMatcher::new("A+G+Shift+Space").unwrap();
        m.on_modifier(Modifier::Shift, true);
        m.on_key(Key::Char('x'), true);
        m.on_key(Key::Char('a'), true);
        m.on_key(Key::Char('g'), true);
        assert!(m.on_key(Key::Space, true));
    }

    #[test]
    fn modifier_only_chord_never_fires() {
        let mut m = ChordMatcher::new("Ctrl+Shift").unwrap();
        assert!(m.chord().keys.is_empty());
        m.on_modifier(Modifier::Ctrl, true);
        m.on_modifier(Modifier::Shift, true);
        assert!(!m.on_key(Key::Char('a'), true));
        assert!(!m.on_key(Key::Space, true));
    }

    #[test]
    fn chord_accessor_roundtrips() {
        let m = ChordMatcher::new("A+G+Shift+Space").unwrap();
        assert_eq!(m.chord(), &parse_chord("A+G+Shift+Space").unwrap());
        assert_eq!(m.chord().keys.len(), 3);
    }

    #[test]
    fn key_up_returns_false_and_releases() {
        let mut m = ChordMatcher::new("Shift+Space").unwrap();
        m.on_modifier(Modifier::Shift, true);
        assert!(m.on_key(Key::Space, true));
        assert!(!m.on_key(Key::Space, false));
        assert!(m.on_key(Key::Space, true));
    }

    /// The two configured chords: the parade hotkey and the editor hotkey.
    fn two_chord_set() -> ChordSet {
        ChordSet::new(&[
            ("A+G+Shift+Space".to_string(), HotkeyAction::Popup),
            ("D+Shift".to_string(), HotkeyAction::Editor),
        ])
        .expect("both specs parse")
    }

    #[test]
    fn shift_d_parses_without_parser_change() {
        // The editor chord needs no parser work: `D+Shift` already yields
        // the Shift modifier plus the `d` key.
        let c = parse_chord("D+Shift").unwrap();
        assert_eq!(c.required_mods, mods(&[Modifier::Shift]));
        assert_eq!(c.keys, vec![Key::Char('d')]);
    }

    #[test]
    fn chord_set_fires_the_matching_action() {
        let mut set = two_chord_set();
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
        set.on_key(Key::Char('d'), false);
        assert_eq!(set.on_key(Key::Char('a'), true), None);
        set.on_key(Key::Char('g'), true);
        assert_eq!(
            set.on_key(Key::Space, true),
            Some(HotkeyAction::Popup),
            "the parade chord must still fire its own action"
        );
    }

    #[test]
    fn chord_set_never_fires_two_actions() {
        // The two chords share the Shift modifier but no key, so one
        // key-down can only ever produce one action.
        let mut set = two_chord_set();
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
        // The editor chord shares no key with the parade chord: the
        // parade matcher must not fire on the same press.
        assert_eq!(set.on_key(Key::Char('a'), true), None);
        assert_eq!(set.on_key(Key::Char('g'), true), None);
        assert_eq!(set.on_key(Key::Space, true), Some(HotkeyAction::Popup));
    }

    #[test]
    fn chord_set_requires_modifiers_first() {
        let mut set = ChordSet::new(&[("D+Shift".to_string(), HotkeyAction::Editor)]).unwrap();
        // D first: the modifier is not held yet, so nothing fires.
        assert_eq!(set.on_key(Key::Char('d'), true), None);
        // Shift afterwards does not fire the chord either.
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(set.on_key(Key::Char('x'), true), None);
        // The right order fires: Shift held, then D down.
        set.on_key(Key::Char('d'), false);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
    }

    #[test]
    fn chord_set_latches_until_release() {
        let mut set = two_chord_set();
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
        // Key repeat must not fire the same chord twice.
        assert_eq!(set.on_key(Key::Char('d'), true), None);
        // Releasing the chord key re-arms it.
        set.on_key(Key::Char('d'), false);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
        // The parade matcher keeps its own latch, independent of the
        // editor matcher.
        set.on_key(Key::Char('a'), true);
        set.on_key(Key::Char('g'), true);
        assert_eq!(set.on_key(Key::Space, true), Some(HotkeyAction::Popup));
        assert_eq!(set.on_key(Key::Space, true), None);
    }

    #[test]
    fn empty_spec_is_skipped() {
        let mut set = ChordSet::new(&[
            ("A+G+Shift+Space".to_string(), HotkeyAction::Popup),
            ("".to_string(), HotkeyAction::Editor),
        ])
        .expect("an empty spec is skipped, not an error");
        assert_eq!(set.entries.len(), 1, "the empty spec must be skipped");
        // The parade chord still works.
        set.on_modifier(Modifier::Shift, true);
        set.on_key(Key::Char('a'), true);
        set.on_key(Key::Char('g'), true);
        assert_eq!(set.on_key(Key::Space, true), Some(HotkeyAction::Popup));
        // The disabled editor chord never fires.
        set.on_key(Key::Space, false);
        assert_eq!(set.on_key(Key::Char('d'), true), None);
    }

    /// `Esc` stays a legal token for the parser: the windows own ESC (it
    /// closes them), and a window-local chord — and these tests — must be
    /// able to spell it (audit F-134). Only the *binding* path refuses it.
    #[test]
    fn esc_stays_a_legal_token_for_the_parser() {
        let c = parse_chord("Control+Win+Esc").unwrap();
        assert_eq!(c.keys, vec![Key::Escape]);
        assert!(
            ChordMatcher::new("Ctrl+Esc").unwrap().binds_escape(),
            "the matcher must report the Esc key"
        );
    }

    /// The hotkey-binding path refuses a chord that binds `Esc` (audit
    /// F-134): ESC is reserved for closing the popup and the editor, so
    /// arming it globally must fail loudly instead of never firing.
    #[test]
    fn the_binding_path_refuses_a_chord_that_binds_esc() {
        let err = ChordSet::new(&[("Ctrl+Esc".to_string(), HotkeyAction::Popup)])
            .expect_err("Esc must not be bindable as a global hotkey");
        assert!(
            matches!(err, ChordError::EscapeBound(_)),
            "the refusal has its own error, got {err}"
        );
        let err = ChordSet::new(&[("A+G+Shift+Escape".to_string(), HotkeyAction::Popup)])
            .expect_err("Esc hides anywhere in the spec");
        assert!(matches!(err, ChordError::EscapeBound(_)));
    }

    /// The binding path still accepts the chords it always did: the
    /// refusal is about `Esc` only.
    #[test]
    fn the_binding_path_accepts_chords_without_esc() {
        let mut set = ChordSet::new(&[
            ("D+Shift".to_string(), HotkeyAction::Editor),
            ("A+G+Shift+Space".to_string(), HotkeyAction::Popup),
        ])
        .expect("no Esc anywhere");
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(set.on_key(Key::Char('d'), true), Some(HotkeyAction::Editor));
    }
}

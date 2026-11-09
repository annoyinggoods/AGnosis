//! The pure core of the ducks ASCII duck parade.
//!
//! Everything in here is **platform-free**: the simulation, the art, the
//! scene, the data model and the exporters. The window, the hotkey, the
//! daemon, the folder watcher and the file layout live in the binary
//! crate, and they hand the core plain data (see `agnosis_core::assets`).
//!
//! The rule this crate exists to enforce: **no OS-conditional `cfg` and no
//! dependency on the platform tree**. A grep proves it — the pattern below
//! finds the platform attribute (`cfg` followed by the operating system's
//! name), and this tree has none:
//!
//! ```text
//! grep -rniE "cfg\s*\(\s*target" crates/agnosis-core/src
//! ```

pub mod assets;
pub mod audio;
pub mod backdrop;
pub mod balloon;
pub mod blit;
pub mod config;
pub mod config_edit;
pub mod content;
pub mod drawn;
pub mod editor;
pub mod export;
pub mod font;
pub mod gait;
pub mod grid;
pub mod help;
pub mod hud_font;
pub mod image_ascii;
pub mod layer;
pub mod manifest;
pub mod package;
pub mod parade;
pub mod path_error;
pub mod pendulum;
pub mod personality;
pub mod preset;
pub mod reaction;
pub mod render;
pub mod rope;
pub mod scene;
pub mod selftest;
pub mod sky;
pub mod sprite_art;
pub mod sprites;
pub mod stats;
pub mod trace;
pub mod weather;

#[cfg(test)]
mod audit_proto;

#[cfg(test)]
mod purity;

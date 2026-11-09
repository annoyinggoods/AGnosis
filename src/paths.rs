//! Platform directory resolution, all via the `directories` crate.
//!
//! One quirk worth remembering, because it has bitten this project before:
//! `ProjectDirs::from("com", "AGnosis", "AGnosis")` repeats the application name
//! on Windows, so the config dir is `%APPDATA%\AGnosis\AGnosis\config` and the
//! balloon folder is `%APPDATA%\AGnosis\AGnosis\config\balloons` — *not*
//! `%APPDATA%\AGnosis\balloons`. Every caller goes through the functions below
//! so the watcher, the CLI and the popup always agree on the location.

use std::path::PathBuf;

/// The path error lives in [`agnosis_core::path_error`], because the **pure**
/// side of the crate must be able to report a path failure without
/// knowing the platform. It is re-exported here so every existing
/// `crate::paths::PathError` reference keeps working.
pub use agnosis_core::path_error::PathError;

/// The whole folder layout under a config root.
///
/// The layout is **pure**: it joins, it never touches the disk. The
/// public helpers resolve the platform's root and create the folders;
/// this one is what the tests use, so `cargo test` never materialises a
/// tree in the user's real config folder (audit 2026-10-21 §G12).
#[derive(Debug, Clone)]
pub struct Layout {
    /// `<config>/balloons` — the watched balloon folder.
    pub balloons: PathBuf,
    /// `<config>/drawn` — the root of the user-drawn assets.
    pub drawn: PathBuf,
    /// `<config>/drawn/sprites`.
    pub sprites: PathBuf,
    /// `<config>/drawn/balloons`.
    pub drawn_balloons: PathBuf,
    /// `<config>/drawn/grass`.
    pub grass: PathBuf,
    /// `<config>/drawn/decorations`.
    pub decorations: PathBuf,
    /// `<config>/drawn/drafts`.
    pub drafts: PathBuf,
    /// `<config>/presets`.
    pub presets: PathBuf,
    /// `<config>/tracks`.
    pub tracks: PathBuf,
    /// `<config>/backdrops`.
    pub backdrops: PathBuf,
    /// `<config>/run` — the PID files.
    #[cfg(any(unix, doc))]
    pub runtime: PathBuf,
}

impl Layout {
    /// The layout under `config` — a pure join, nothing is created.
    pub fn under(config: &std::path::Path) -> Self {
        let drawn = config.join("drawn");
        Self {
            balloons: config.join("balloons"),
            sprites: drawn.join("sprites"),
            drawn_balloons: drawn.join("balloons"),
            grass: drawn.join("grass"),
            decorations: drawn.join("decorations"),
            drafts: drawn.join("drafts"),
            drawn,
            presets: config.join("presets"),
            tracks: config.join("tracks"),
            backdrops: config.join("backdrops"),
            #[cfg(any(unix, doc))]
            runtime: config.join("run"),
        }
    }
}

/// The config root, **without** creating it.
///
/// The public [`config_dir`] creates the folder; this one only resolves
/// it, so the tests can check the layout without touching the user's
/// real config folder (audit 2026-10-21 §G12).
pub fn config_root() -> Result<PathBuf, PathError> {
    Ok(directories::ProjectDirs::from("com", "AGnosis", "AGnosis")
        .ok_or(PathError::NoConfigDir)?
        .config_dir()
        .to_path_buf())
}

/// `%APPDATA%\AGnosis\AGnosis\config`, `~/Library/Application Support/com.agnosis.agnosis`,
/// `~/.config/AGnosis`.
pub fn config_dir() -> Result<PathBuf, PathError> {
    let dir = config_root()?;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

pub fn config_file() -> Result<PathBuf, PathError> {
    Ok(config_dir()?.join("config.toml"))
}

/// `<config>/balloons/` — the folder the watcher scans for balloon art.
///
/// On Windows this is `%APPDATA%\AGnosis\AGnosis\config\balloons`: the
/// `directories` crate repeats the application name in the project path.
pub fn balloon_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&config_dir()?).balloons;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/` — the root of the user-drawn assets.
///
/// Drawn art lives in its own tree (roadmap decision D5): the balloon folder
/// is watched and its `.ans` path is single-coloured, so user art must not
/// go there.
pub fn drawn_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&config_dir()?).drawn;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/sprites/` — the user-drawn walking sprites.
pub fn drawn_sprites_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&drawn_dir()?).sprites;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/balloons/` — the user-drawn balloons.
pub fn drawn_balloons_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&drawn_dir()?).drawn_balloons;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/drafts/` — the editor's autosaved drafts.
///
/// A draft is **not** an asset: it lives in its own folder, one level
/// below the asset folders, so the scene and `agnosis list-drawn` can never
/// pick a half-finished drawing up as if it were a real asset
/// (roadmap Extension 2 §2.2).
pub fn drafts_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&drawn_dir()?).drafts;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/decorations/` — the user-drawn duck decorations.
///
/// A decoration is a second frame-sized asset per duck size class, drawn
/// over the duck's body (roadmap Extension 2 §4.3).
pub fn drawn_decorations_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&drawn_dir()?).decorations;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/presets/` — the saved scene presets.
pub fn presets_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&config_dir()?).presets;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/tracks/` — the folder the MIDI note tracks live in.
///
/// A track is a file the user creates (or the editor saves), so it sits
/// beside the presets and the config, not inside `<config>/drawn/`.
pub fn tracks_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&config_dir()?).tracks;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/drawn/grass/` — the user-drawn grass tiles.
///
/// A grass tile is a **tile**, not a frame, so it lives in its own folder
/// beside the sprites and the balloons.
pub fn drawn_grass_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&drawn_dir()?).grass;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// `<config>/backdrops` — the pictures the parallax layers are built from.
///
/// A backdrop is a **picture**, not a drawn asset, so it lives in its own
/// folder: the conversion turns it into an ASCII tile, and the drawn-asset
/// folders keep their own formats.
///
/// The folder is **not** created: the resolver only joins a name onto it,
/// so it must not materialise the folder as a side effect (audit
/// 2026-10-21 §G12).
pub fn backdrops_root() -> Result<PathBuf, PathError> {
    Ok(Layout::under(&config_root()?).backdrops)
}

/// `<config>/drawn/balloons` — **without** creating it.
///
/// The balloon lookup only reads the folder, so it must not materialise
/// it (audit 2026-10-21 §G23).
pub fn drawn_balloons_root() -> Result<PathBuf, PathError> {
    Ok(Layout::under(&config_root()?).drawn_balloons)
}

/// Does a relative name try to leave the folder it is joined onto?
///
/// A name with a `..` component — `../../secrets.png` — resolves
/// **outside** the folder, so the relative form is refused. An
/// **absolute** name is deliberately allowed: the user may point at a
/// file anywhere (audit 2026-10-21 §G17).
pub fn escapes_its_folder(name: &str) -> bool {
    std::path::Path::new(name)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// Runtime scratch dir for the PID files: `<config>/run/`.
///
/// Unix only: the daemon and the popup own a PID file there, while the
/// Windows popup is found by window class (`FindWindowW`) and the Windows
/// daemon by its named stop event — neither writes a PID file. Gating the
/// helper keeps the Windows build free of dead code. `doc` keeps the item
/// visible to rustdoc so the `crate::paths::popup_pid_file` references in
/// `platform::window` still resolve.
#[cfg(any(unix, doc))]
pub fn runtime_dir() -> Result<PathBuf, PathError> {
    let dir = Layout::under(&config_dir()?).runtime;
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// The daemon's exit-reason file: `<config>/run/exit-reason.txt`.
///
/// Cross-platform on purpose (audit 2026-10-23 §F-100): a detached,
/// stdio-nulled daemon has no channel to the terminal on any OS, so the
/// reason lands here and `agnosis start` prints it when the daemon it just
/// spawned is gone a second later.
pub fn exit_reason_file() -> Result<PathBuf, PathError> {
    // The same `<config>/run/` folder `runtime_dir` owns on Unix; spelled
    // out here because that helper (and the Layout field) is Unix-gated
    // and the reason file is needed on every OS.
    let dir = config_dir()?.join("run");
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir.join("exit-reason.txt"))
}

/// PID file of the running popup: `<config>/run/popup.pid`.
///
/// Unix only, for the same reason as [`runtime_dir`]: the Windows popup is
/// located by window class, not by PID file.
#[cfg(any(unix, doc))]
pub fn popup_pid_file() -> Result<PathBuf, PathError> {
    Ok(runtime_dir()?.join("popup.pid"))
}

/// PID file of the running editor: `<config>/run/editor.pid`.
///
/// Unix only, for the same reason as [`popup_pid_file`]: the Windows editor
/// is located by window class (`FindWindowW("AGnosisEditor")`), not by PID
/// file. The editor has its own file, so the popup's toggle never signals
/// the editor and vice versa (roadmap Stage 4).
#[cfg(any(unix, doc))]
pub fn editor_pid_file() -> Result<PathBuf, PathError> {
    Ok(runtime_dir()?.join("editor.pid"))
}

/// PID file of the running HELP window: `<config>/run/help.pid`.
///
/// Unix only, for the same reason as [`editor_pid_file`]: the Windows
/// HELP window is located by window class (`FindWindowW("AGnosisHelp")`),
/// not by PID file. The file is the HELP window's **single-instance
/// claim** (audit 2026-10-25): the help process claims it before its
/// window exists, and a second spawn exits instead of stacking another
/// reader over the first.
#[cfg(any(unix, doc))]
pub fn help_pid_file() -> Result<PathBuf, PathError> {
    Ok(runtime_dir()?.join("help.pid"))
}

#[cfg(target_os = "macos")]
pub fn autostart_plist_path() -> Result<PathBuf, PathError> {
    let home = directories::BaseDirs::new()
        .ok_or(PathError::NoConfigDir)?
        .home_dir()
        .to_path_buf();
    let dir = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    // The plist is named after the LaunchAgent label, so the two cannot
    // drift apart.
    Ok(dir.join(format!("{}.plist", crate::platform::APP_BUNDLE_ID)))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn autostart_desktop_path() -> Result<PathBuf, PathError> {
    let home = directories::BaseDirs::new()
        .ok_or(PathError::NoConfigDir)?
        .home_dir()
        .to_path_buf();
    let dir = home.join(".config").join("autostart");
    std::fs::create_dir_all(&dir).map_err(|source| PathError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir.join(crate::platform::AUTOSTART_DESKTOP_NAME))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
pub fn autostart_desktop_path() -> Result<PathBuf, PathError> {
    Err(PathError::NoConfigDir)
}

/// Current executable path, used to re-exec ourselves for the popup.
pub fn current_exe() -> Result<PathBuf, PathError> {
    std::env::current_exe().map_err(|source| PathError::Io {
        path: PathBuf::from("<current_exe>"),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp root for the layout tests — nothing is created in the
    /// user's real config folder (audit 2026-10-21 §G12).
    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "agnosis-paths-{}-{nanos}-{tag}",
            std::process::id()
        ))
    }

    #[test]
    fn config_and_balloon_dirs_are_ducks_scoped() {
        // The **root** is resolved without creating it, so the test never
        // materialises a tree in the user's real config folder.
        let c = config_root().expect("config root");
        assert!(
            c.to_string_lossy().contains("AGnosis"),
            "got {}",
            c.display()
        );
        let layout = Layout::under(&c);
        assert!(layout.balloons.starts_with(&c));
        assert_eq!(layout.balloons, c.join("balloons"));
    }

    #[test]
    fn drawn_dirs_sit_under_the_config_dir() {
        let root = temp_root("drawn");
        let layout = Layout::under(&root);
        assert!(layout.drawn.starts_with(&root));
        for dir in [
            &layout.sprites,
            &layout.drawn_balloons,
            &layout.grass,
            &layout.decorations,
            &layout.drafts,
        ] {
            assert!(dir.starts_with(&layout.drawn), "got {}", dir.display());
        }
    }

    /// The tracks folder sits beside the config, not inside the drawn
    /// assets: a track is a sound file, not a drawing.
    #[test]
    fn the_tracks_folder_sits_under_the_config_dir() {
        let root = temp_root("tracks");
        let layout = Layout::under(&root);
        assert!(
            layout.tracks.starts_with(&root),
            "got {}",
            layout.tracks.display()
        );
        assert!(!layout.tracks.starts_with(&layout.drawn));
    }

    /// A draft must never sit in an asset folder: the scene and
    /// `agnosis list-drawn` scan those, and a half-finished drawing must
    /// never be picked up as if it were a real asset.
    #[test]
    fn the_drafts_folder_is_its_own() {
        let root = temp_root("drafts");
        let layout = Layout::under(&root);
        assert!(layout.drafts.starts_with(&layout.drawn));
        for asset_dir in [
            &layout.sprites,
            &layout.drawn_balloons,
            &layout.grass,
            &layout.decorations,
        ] {
            assert!(
                !layout.drafts.starts_with(asset_dir),
                "got {}",
                layout.drafts.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn runtime_pid_file_under_config() {
        let root = temp_root("pid");
        let layout = Layout::under(&root);
        let p = layout.runtime.join("popup.pid");
        assert!(p.file_name().unwrap() == "popup.pid");
        assert!(p.starts_with(&root));
    }

    #[cfg(unix)]
    #[test]
    fn editor_pid_file_is_its_own_file_under_config() {
        let root = temp_root("editor-pid");
        let layout = Layout::under(&root);
        let p = layout.runtime.join("editor.pid");
        assert!(p.file_name().unwrap() == "editor.pid");
        assert!(p.starts_with(&root));
        // The editor and the popup never share a PID file: the popup's
        // toggle must not signal the editor (roadmap Stage 4).
        assert_ne!(p, layout.runtime.join("popup.pid"));
    }

    #[test]
    fn current_exe_resolves() {
        assert!(current_exe().is_ok());
    }
}

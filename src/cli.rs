//! Command-line dispatch for the single `agnosis` binary.
//!
//! Dispatch table: `agnosis` renders the popup on the configured monitor,
//! `agnosis --daemon` runs the resident hotkey loop, `install`/`uninstall`
//! manage login autostart, `start`/`stop` manage the running daemon, `add`
//! and `add-from-text` convert art into balloon sprites, `assign` points a
//! parade slot at a drawn asset, `list-drawn` prints the drawn assets,
//! `selftest` renders headless frames to PNG, and the hidden `popup` and
//! `editor` subcommands are the spawn targets the daemon execs per trigger.

use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

/// `ducks` — global hotkey that hides the browser behind a duck parade.
#[derive(Debug, Parser)]
#[command(name = "agnosis", version, about)]
// `agnosis help` is **our** verb — the in-app HELP window (roadmap
// HELP_WINDOW §1) — so clap's auto-generated `help` subcommand is
// disabled. The `-h`/`--help` flags and every `agnosis <cmd> --help` keep
// working; only the two-word listing form is gone.
#[command(disable_help_subcommand = true)]
struct Cli {
    /// Run the resident hotkey daemon instead of the popup.
    #[arg(long)]
    daemon: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

/// What `agnosis config` does.
#[derive(Subcommand, Debug)]
pub(crate) enum ConfigCommands {
    /// Print the effective configuration — the file after the repair pass.
    Show,
    /// Rewrite the configuration with the default values.
    ///
    /// Every value the user changed is **gone** — the colours, the
    /// hotkeys, the assignments, the personalities. The previous file is
    /// kept as `config.toml.bak`, so nothing is lost *for good*, but the
    /// reset itself is refused until `--yes` names it
    /// (audit 2026-10-22 §UX-02).
    Reset {
        /// Confirm the reset: without this flag nothing is written.
        #[arg(long)]
        yes: bool,
    },
    /// Copy the current configuration to a timestamped file.
    ///
    /// Deliberately not the `.bak` (which the writer overwrites): the user
    /// wants a *named* history.
    Backup,
    /// Parse the configuration and report: repairs, or the parse error.
    ///
    /// The live reload refuses a broken file **silently** (one trace line
    /// and a message on the parade's input line); this verb is the way to
    /// ask the same question from a terminal (audit 2026-10-23 §F-097).
    Validate,
    /// Write the configuration's manifest to a file.
    Export {
        /// Where to write the manifest.
        file: PathBuf,
    },
}

/// What `agnosis preset` does.
#[derive(Subcommand, Debug)]
pub(crate) enum PresetCommands {
    /// Save the current look under a name.
    Save {
        /// The preset's name.
        name: String,
    },
    /// List the saved presets.
    List,
    /// Apply a saved preset.
    Use {
        /// The preset's name.
        name: String,
    },
}

/// What `agnosis track` does.
#[derive(Subcommand, Debug)]
pub(crate) enum TrackCommands {
    /// List the saved tracks.
    List,
    /// Print the notes of a track.
    Show {
        /// The track's name.
        name: String,
    },
    /// Make the parade play a track.
    Use {
        /// The track's name.
        name: String,
    },
    /// Stop playing a track.
    Clear,
}

/// Subcommands of the `agnosis` binary.
#[derive(Debug, Subcommand)]
enum Commands {
    /// Register `agnosis --daemon` to run at login.
    Install,
    /// Remove the login entry and stop the daemon, keeping balloons.
    Uninstall,
    /// Spawn a detached daemon unless one is already running.
    Start,
    /// Ask a running daemon to stop.
    Stop,
    /// Convert an image into a balloon sprite.
    Add {
        /// Source image file (PNG/JPEG/GIF/BMP, content-sniffed).
        image: PathBuf,
    },
    /// Convert a hand-made `.ans` file into a balloon sprite.
    #[command(name = "add-from-text")]
    AddFromText {
        /// Hand-made ASCII art file.
        file: PathBuf,
    },
    /// Render headless frames to PNG without a window or GPU.
    Selftest {
        /// Frames to render.
        #[arg(long, default_value_t = 30)]
        frames: u32,
        /// Output directory for the PNG frames.
        #[arg(long, default_value = "./selftest_out")]
        out: PathBuf,
        /// Render this configuration instead of the built-in one.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Assign a drawn asset to a parade slot.
    Assign {
        /// Slot: BIG1..BIG2, SMALL1..SMALL9, BALLOON1..BALLOON5.
        slot: String,
        /// Asset name (omit with --clear to remove the assignment).
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// List the assets you have drawn.
    ListDrawn,
    /// Write a shareable file: a package, an HTML page or a GIF.
    ///
    /// The **extension** picks the format: `.html` writes the
    /// self-contained page, `.gif` writes the animated GIF, and every
    /// other extension writes the package (the drawn art, the
    /// assignments and the look).
    Export {
        /// Where to write the file.
        file: PathBuf,
        /// The frames per second of the HTML and the GIF export.
        #[arg(long, default_value_t = agnosis_core::export::frames::DEFAULT_FPS)]
        fps: u32,
        /// The frames to record. Defaults to one loop of the parade.
        #[arg(long)]
        frames: Option<u32>,
        /// The GIF's downscale factor: 1 is the app's own size.
        #[arg(long, default_value_t = 2)]
        scale: u32,
        /// The creation's name in the manifest / the page's title.
        ///
        /// The default is `agnosis package` (and the page's own default):
        /// a name of your own keeps the files you share apart
        /// (audit 2026-10-23 §F-126 — the name used to be hard-coded).
        #[arg(long)]
        name: Option<String>,
    },
    /// Import a package into this machine's AGnosis folders.
    Import {
        /// The package to import.
        file: PathBuf,
    },
    /// Save, list and apply scene presets.
    Preset {
        /// What to do with the presets.
        #[command(subcommand)]
        command: PresetCommands,
    },
    /// List, show and choose the MIDI note tracks.
    Track {
        /// What to do with the tracks.
        #[command(subcommand)]
        command: TrackCommands,
    },
    /// Write a frame of the parade to a PNG file.
    Shot {
        /// Where to write the PNG.
        file: PathBuf,
    },
    /// Use a drawn grass tile for the ground band.
    Grass {
        /// The tile's name (omit with --clear to go back to the built-in
        /// band).
        name: Option<String>,
        #[arg(long)]
        clear: bool,
    },
    /// Show or reset the configuration.
    Config {
        /// What to do with the configuration.
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Run the macOS menu-bar trigger (macOS only).
    ///
    /// The menu bar is the permission-free way to open the parade and
    /// the editor: it needs no Input Monitoring grant, so it works
    /// even when the daemon refuses to start. Every other platform
    /// answers with "the menu bar is macOS only".
    Menubar,
    /// Open the in-app HELP window: the full README, scrollable.
    ///
    /// The window reads `README.md` **at open time** — beside the
    /// binary, or beside the repo checkout for development builds — so
    /// the documentation the window shows is always the documentation
    /// that ships, with no second copy to keep in sync. A missing file
    /// falls back to the copy embedded at build time (roadmap
    /// HELP_WINDOW §1).
    ///
    /// The monitor geometry is optional: the parade's F1 spawn passes
    /// its own monitor so the reader opens beside the parade; a bare
    /// `agnosis help` resolves the configured monitor instead.
    Help {
        /// Monitor origin in physical pixels, `X,Y`. Omitted: resolved
        /// from the config.
        #[arg(long, value_parser = parse_pair_i32, allow_hyphen_values = true)]
        origin: Option<(i32, i32)>,
        /// Monitor size in physical pixels, `W,H`. Omitted: resolved
        /// from the config.
        #[arg(long, value_parser = parse_pair_u32)]
        size: Option<(u32, u32)>,
    },
    /// Spawn target the daemon execs per trigger (hidden from help).
    #[command(hide = true)]
    Popup {
        /// Monitor origin in physical pixels, `X,Y`.
        #[arg(long, value_parser = parse_pair_i32, allow_hyphen_values = true)]
        origin: (i32, i32),
        /// Monitor size in physical pixels, `W,H`.
        #[arg(long, value_parser = parse_pair_u32)]
        size: (u32, u32),
        /// DPI scale factor.
        #[arg(long)]
        scale: f64,
    },
    /// Spawn target the daemon execs for the draw-your-own editor (hidden).
    #[command(hide = true)]
    Editor {
        /// Monitor origin in physical pixels, `X,Y`.
        #[arg(long, value_parser = parse_pair_i32, allow_hyphen_values = true)]
        origin: (i32, i32),
        /// Monitor size in physical pixels, `W,H`.
        #[arg(long, value_parser = parse_pair_u32)]
        size: (u32, u32),
    },
}

/// Parse `std::env::args_os()`, dispatch, return the exit code.
pub fn run() -> i32 {
    // Belt-and-braces with the build-time PerMonitorV2 manifest (build.rs):
    // a manifest can be stripped by external linkers/packers, so also raise
    // DPI awareness at runtime before any monitor query or winit init. This
    // is a no-op once the manifest already applied (same awareness level).
    crate::platform::window::ensure_dpi_awareness();
    match run_inner(std::env::args_os()) {
        Ok(code) => code,
        Err(code) => code,
    }
}

/// Shared popup target resolution for the default popup path and the
/// daemon trigger path so both resolve identically.
pub(crate) fn resolve_popup_target(
    cfg: &agnosis_core::config::Config,
) -> Result<crate::popup::PopupArgs, String> {
    monitor::resolve(cfg)
}

/// Parsed CLI for tests: mirrors [`run_inner`] dispatch without side effects.
#[cfg(test)]
pub(crate) fn parse_args(args: Vec<String>) -> Result<ParsedCli, String> {
    let owned: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    let cli = Cli::try_parse_from(owned).map_err(|e| e.to_string())?;
    ParsedCli::from_cli(cli)
}

/// Test view over a parsed command line.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ParsedCli {
    #[allow(dead_code)]
    daemon: bool,
    kind: ParsedKind,
}

/// Test view over the parsed subcommand.
#[cfg(test)]
#[derive(Debug)]
pub(crate) enum ParsedKind {
    /// No subcommand: default popup path.
    DefaultPopup,
    /// `--daemon` flag without a subcommand.
    Daemon,
    Install,
    Uninstall,
    Start,
    Stop,
    Add(PathBuf),
    #[allow(dead_code)]
    AddFromText(PathBuf),
    Selftest {
        frames: u32,
        out: PathBuf,
        #[allow(dead_code)]
        config: Option<PathBuf>,
    },
    Assign {
        slot: String,
        name: Option<String>,
        clear: bool,
    },
    ListDrawn,
    #[allow(dead_code)]
    Export {
        file: PathBuf,
        fps: u32,
        frames: Option<u32>,
        scale: u32,
        name: Option<String>,
    },
    #[allow(dead_code)]
    Import(PathBuf),
    #[allow(dead_code)]
    Preset(PresetCommands),
    #[allow(dead_code)]
    Track(TrackCommands),
    #[allow(dead_code)]
    Shot(PathBuf),
    #[allow(dead_code)]
    Grass {
        name: Option<String>,
        clear: bool,
    },
    Menubar,
    /// The HELP verb. The optional monitor geometry is carried so a test
    /// can see what the spawner passed; production dispatches on the
    /// `Commands` value directly.
    #[allow(dead_code)]
    Help {
        origin: Option<(i32, i32)>,
        size: Option<(u32, u32)>,
    },
    /// The config subcommand. The payload is carried so a test can see
    /// which half was asked for; production dispatches on it directly.
    Config(ConfigCommands),
    Popup(crate::popup::PopupArgs),
    Editor(crate::editor::EditorArgs),
}

#[cfg(test)]
impl ParsedCli {
    fn from_cli(cli: Cli) -> Result<Self, String> {
        if cli.daemon {
            return Ok(Self {
                daemon: true,
                kind: ParsedKind::Daemon,
            });
        }
        let kind = match cli.command {
            None => ParsedKind::DefaultPopup,
            Some(Commands::Install) => ParsedKind::Install,
            Some(Commands::Uninstall) => ParsedKind::Uninstall,
            Some(Commands::Start) => ParsedKind::Start,
            Some(Commands::Stop) => ParsedKind::Stop,
            Some(Commands::Add { image }) => ParsedKind::Add(image),
            Some(Commands::AddFromText { file }) => ParsedKind::AddFromText(file),
            Some(Commands::Selftest {
                frames,
                out,
                config,
            }) => ParsedKind::Selftest {
                frames,
                out,
                config,
            },
            Some(Commands::Assign { slot, name, clear }) => {
                ParsedKind::Assign { slot, name, clear }
            }
            Some(Commands::ListDrawn) => ParsedKind::ListDrawn,
            Some(Commands::Export {
                file,
                fps,
                frames,
                scale,
                name,
            }) => ParsedKind::Export {
                file,
                fps,
                frames,
                scale,
                name,
            },
            Some(Commands::Import { file }) => ParsedKind::Import(file),
            Some(Commands::Preset { command }) => ParsedKind::Preset(command),
            Some(Commands::Track { command }) => ParsedKind::Track(command),
            Some(Commands::Shot { file }) => ParsedKind::Shot(file),
            Some(Commands::Grass { name, clear }) => ParsedKind::Grass { name, clear },
            Some(Commands::Menubar) => ParsedKind::Menubar,
            Some(Commands::Help { origin, size }) => ParsedKind::Help { origin, size },
            Some(Commands::Config { command }) => ParsedKind::Config(command),
            Some(Commands::Popup {
                origin,
                size,
                scale,
            }) => ParsedKind::Popup(crate::popup::PopupArgs {
                origin,
                size,
                scale,
            }),
            Some(Commands::Editor { origin, size }) => {
                ParsedKind::Editor(crate::editor::EditorArgs { origin, size })
            }
        };
        Ok(Self {
            daemon: false,
            kind,
        })
    }

    /// Popup args carried by a hidden `popup` invocation.
    pub(crate) fn popup_args_for_test(&self) -> &crate::popup::PopupArgs {
        match &self.kind {
            ParsedKind::Popup(args) => args,
            other => panic!("expected popup invocation, got {other:?}"),
        }
    }

    /// Subcommand kind for parser assertions.
    pub(crate) fn kind(&self) -> &ParsedKind {
        &self.kind
    }
}

/// Parse one command line; errors print via clap and map to exit code 2.
fn run_inner(args: impl IntoIterator<Item = OsString>) -> Result<i32, i32> {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            use std::io::Write;
            let code = e.exit_code();
            let _ = e.print();
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            return Err(code);
        }
    };
    if cli.daemon {
        return Ok(run_daemon_or_report_mix(&cli));
    }
    Ok(match cli.command {
        None => run_default_popup(),
        Some(Commands::Install) => run_install(),
        Some(Commands::Uninstall) => run_uninstall(),
        Some(Commands::Start) => run_start(),
        Some(Commands::Stop) => run_stop(),
        Some(Commands::Add { image }) => run_add(&image),
        Some(Commands::AddFromText { file }) => run_add_from_text(&file),
        Some(Commands::Selftest {
            frames,
            out,
            config,
        }) => run_selftest(frames, &out, config.as_deref()),
        Some(Commands::Assign { slot, name, clear }) => run_assign(&slot, name.as_deref(), clear),
        Some(Commands::ListDrawn) => run_list_drawn(),
        Some(Commands::Export {
            file,
            fps,
            frames,
            scale,
            name,
        }) => run_export(&file, fps, frames, scale, name.as_deref()),
        Some(Commands::Import { file }) => run_import(&file),
        Some(Commands::Preset { command }) => run_preset(command),
        Some(Commands::Track { command }) => run_track(command),
        Some(Commands::Shot { file }) => run_shot(&file),
        Some(Commands::Grass { name, clear }) => run_grass(name.as_deref(), clear),
        Some(Commands::Config { command }) => run_config(command),
        Some(Commands::Menubar) => run_menubar(),
        Some(Commands::Help { origin, size }) => run_help(origin, size),
        Some(Commands::Popup {
            origin,
            size,
            scale,
        }) => run_popup(origin, size, scale),
        Some(Commands::Editor { origin, size }) => run_editor(origin, size),
    })
}

/// Load config or report and return a non-zero exit code.
fn load_config() -> Result<agnosis_core::config::Config, i32> {
    crate::host::load_config().map_err(|e| {
        eprintln!("agnosis: config error: {e}");
        1
    })
}

/// `agnosis` with no args: popup renderer with the configured monitor.
fn run_default_popup() -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let args = match monitor::resolve(&cfg) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("agnosis: no popup target: {e}");
            return 1;
        }
    };
    match crate::popup::run(&cfg, &args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agnosis: popup failed: {e}");
            1
        }
    }
}

/// `agnosis --daemon`: resident hotkey loop.
///
/// The flag used to swallow the subcommand silently: `agnosis --daemon
/// install` started the daemon and ignored `install` (audit 2026-10-20
/// §3.18). A subcommand and the flag are mutually exclusive, so the
/// mistake is reported here — and the check lives in its own function so
/// the guard does not nest an `if` inside an `if` (audit 2026-10-21 §G2).
fn run_daemon_or_report_mix(cli: &Cli) -> i32 {
    if cli.command.is_some() {
        eprintln!("agnosis: --daemon cannot be combined with a subcommand");
        eprintln!("agnosis: run the subcommand on its own");
        return 2;
    }
    run_daemon()
}

/// `agnosis --daemon`: resident hotkey loop.
fn run_daemon() -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    match crate::daemon::run(&cfg) {
        Ok(()) => 0,
        Err(e @ crate::daemon::DaemonError::AlreadyRunning(_)) => {
            eprintln!("agnosis: {e}");
            1
        }
        Err(e) => {
            eprintln!("agnosis: daemon failed: {e}");
            1
        }
    }
}

/// `agnosis install`: register login autostart with confirmation.
fn run_install() -> i32 {
    match crate::platform::autostart::install() {
        Ok(()) => {
            println!(
                "agnosis: installed autostart for `{}`",
                crate::platform::autostart::autostart_target()
            );
            print_install_hint();
            0
        }
        Err(e) => {
            eprintln!("agnosis: install failed: {e}");
            1
        }
    }
}

/// `agnosis uninstall`: remove the login entry, keep the balloon folder.
fn run_uninstall() -> i32 {
    match crate::platform::autostart::uninstall() {
        Ok(()) => {
            println!("agnosis: autostart removed (balloons kept)");
            0
        }
        Err(e) => {
            eprintln!("agnosis: uninstall failed: {e}");
            1
        }
    }
}

/// The extra line a macOS install owes the user.
///
/// On macOS the LaunchAgent starts `agnosis menubar` — the menu bar needs
/// no permission — and the **global hotkey** is the daemon's, which is
/// not running until `agnosis start` says so. Nothing in the install
/// output said that, and a README-less user concluded the hotkey was
/// broken (audit 2026-10-23 §F-099).
#[cfg(target_os = "macos")]
fn print_install_hint() {
    println!("agnosis: the menu bar is registered; run `agnosis start` once for the global hotkey");
}

/// Windows and Linux need the same line macOS gets: `install` only
/// registers login autostart, and the **global hotkey** — the browser
/// minimization with it — is the daemon's, which is not running until
/// `agnosis start` says so. The macOS gap was audit 2026-10-23 §F-099; a
/// Windows user hit the identical wall (install printed nothing, and a
/// README-less user concluded the hotkey was broken).
#[cfg(not(target_os = "macos"))]
fn print_install_hint() {
    println!("agnosis: autostart registered; run `agnosis start` once for the global hotkey");
}

/// `agnosis start`: spawn a detached daemon unless one is already armed.
fn run_start() -> i32 {
    if daemon_running() {
        println!("agnosis: daemon already running");
        return 0;
    }
    let exe = match crate::paths::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("agnosis: cannot locate executable: {e}");
            return 1;
        }
    };
    let mut command = std::process::Command::new(exe);
    command.arg("--daemon");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{CREATE_NO_WINDOW, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS.0 | CREATE_NO_WINDOW.0);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // A stale reason from an earlier daemon must not be printed for this
    // one: the file is cleared before the spawn, so whatever
    // [`report_daemon_exit_reason`] reads later is this daemon's own
    // words (audit 2026-10-23 §F-100).
    crate::daemon::clear_exit_reason();
    match command.spawn() {
        Ok(_) => {
            // Wait for the daemon to arm its stop record, so a daemon
            // that died on startup is reported instead of being invisible
            // (audit 2026-10-20 §3.6). The child's stdio is nulled, so
            // its own error message cannot reach the terminal — this is
            // the only honest signal the caller gets.
            if !wait_for_daemon(DAEMON_START_TIMEOUT) {
                eprintln!("agnosis: the daemon did not come up");
                eprintln!(
                    "agnosis: it may have failed to start (set AGNOSIS_TRACE_FILE to see why)"
                );
                return 1;
            }
            report_daemon_exit_reason()
        }
        Err(e) => {
            eprintln!("agnosis: could not start daemon: {e}");
            1
        }
    }
}

/// Re-check the daemon a moment after its stop record appeared.
///
/// The record is armed **before** the hotkey listener spawns, so a
/// daemon that dies right after arming (the macOS Input Monitoring
/// refusal, a broken chord) used to be reported as "daemon started"
/// (audit 2026-10-23 §F-100). The daemon now writes its exit reason to
/// a tiny file — the only channel a detached, stdio-nulled process has
/// — and this prints it when the daemon is gone a second later.
fn report_daemon_exit_reason() -> i32 {
    std::thread::sleep(DAEMON_LIVENESS_RECHECK);
    if daemon_running() {
        println!("agnosis: daemon started");
        return 0;
    }
    let reason =
        crate::daemon::read_exit_reason().unwrap_or_else(|| "no exit reason was recorded".into());
    eprintln!("agnosis: the daemon exited: {reason}");
    1
}

/// How long `agnosis start` waits before it re-checks liveness.
const DAEMON_LIVENESS_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

/// How long `agnosis start` waits for the daemon to arm its stop record.
const DAEMON_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Wait for the daemon to become detectable.
///
/// The daemon arms its stop record (the named event on Windows, the PID
/// file on Unix) before it does anything else, so the record appearing is
/// the daemon's own "I am up" signal.
fn wait_for_daemon(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if daemon_running() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// `agnosis stop`: signal a running daemon.
///
/// Reports each outcome honestly (audit P0-2): a stale PID file that was
/// cleaned up is not the same as "nothing was running".
///
/// The exit code follows the outcome: 0 when a daemon was signalled (or a
/// stale record was cleaned up — there *was* something to clean), 1 when
/// nothing was running. A script can therefore tell "stopped it" from
/// "there was nothing there" (audit 2026-10-20 §3.18).
fn run_stop() -> i32 {
    use crate::platform::daemon_ctl::StopOutcome;
    match crate::platform::daemon_ctl::request_stop_status() {
        StopOutcome::Signalled => {
            println!("agnosis: stop requested");
            0
        }
        StopOutcome::StaleCleaned => {
            println!("agnosis: no daemon running (stale pid file removed)");
            0
        }
        StopOutcome::NotRunning => {
            println!("agnosis: no daemon running");
            1
        }
    }
}

/// `agnosis add <image>`: convert with the live monitor radii, cache the sprite.
///
/// Conversion runs at the resolved popup monitor's radii (audit §P0-3): the
/// `.sprite.json` cache wins over re-conversion in the watcher, so baking
/// 1080p art on a 4K panel would show the wrong size. Falls back to the
/// historical 1920x1080 with a warn trace when no monitor resolves.
///
/// The art is copied into the balloon folder when it lives elsewhere and the
/// cache is written next to it, so the watcher picks the pair up.
fn run_add(image: &std::path::Path) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let (rx, ry) = live_add_radii(&cfg);
    let sprite = match agnosis_core::image_ascii::convert_image_file(image, rx, ry) {
        Ok(sprite) => sprite,
        Err(e) => {
            eprintln!("agnosis: cannot convert {}: {e}", image.display());
            return 1;
        }
    };
    finish_add(image, &sprite, (rx, ry))
}

/// `agnosis add-from-text <file.ans>`: convert hand-made art, cache the sprite.
fn run_add_from_text(file: &std::path::Path) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let (rx, ry) = live_add_radii(&cfg);
    let sprite = match agnosis_core::image_ascii::convert_ans_file(
        file,
        rx,
        ry,
        agnosis_core::image_ascii::ANS_COLOR,
    ) {
        Ok(sprite) => sprite,
        Err(e) => {
            eprintln!("agnosis: cannot convert {}: {e}", file.display());
            return 1;
        }
    };
    finish_add(file, &sprite, (rx, ry))
}

/// Persist a converted `add` sprite and report it. The stamp records the
/// radii the art was built for so the watcher reuses it only on matching
/// geometry (audit §P0-3).
fn finish_add(
    source: &std::path::Path,
    sprite: &agnosis_core::balloon::AsciiSprite,
    radii: (i64, i64),
) -> i32 {
    match cache_sprite(source, sprite, radii) {
        Ok(path) => {
            println!("agnosis: added balloon {}", path.display());
            0
        }
        Err(e) => {
            eprintln!("agnosis: cannot save sprite: {e}");
            1
        }
    }
}

/// The most frames `agnosis selftest` will render.
///
/// The frame count was an unbounded `u32`, so `--frames 4294967295` was an
/// infinite disk-filling loop: every frame is a full-screen PNG (audit
/// 2026-10-20 §3.13). The cap is generous — two minutes of frames at 30
/// fps.
const MAX_SELFTEST_FRAMES: u32 = 3600;

/// `agnosis selftest`: headless frames to PNG without a window or GPU.
fn run_selftest(frames: u32, out: &std::path::Path, config: Option<&std::path::Path>) -> i32 {
    if frames > MAX_SELFTEST_FRAMES {
        eprintln!("agnosis: {frames} frames is over the {MAX_SELFTEST_FRAMES} frame limit");
        return 2;
    }
    // The render gate runs on the **built-in** configuration, never on the
    // user's file: the gate must be reproducible on every machine, and
    // the user's config may carry a date-dependent counter or a
    // different sky. Pass `--config` to render a specific file.
    let mut cfg = match selftest_config(config) {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    // The core reads **absolute** picture paths: the name of a backdrop
    // is a platform question, so the shell answers it here.
    crate::host::resolve_backdrop_paths(&mut cfg);
    // The drawn art and the tracks are the platform's folders: the core
    // never looks them up, so the shell hands them over.
    match agnosis_core::selftest::run(&cfg, frames, out, &crate::host::asset_dirs(&cfg)) {
        Ok(rendered) => {
            println!(
                "agnosis: selftest rendered {rendered} frames to {}",
                out.display()
            );
            0
        }
        Err(e) => {
            eprintln!("agnosis: selftest failed: {e}");
            1
        }
    }
}

/// The configuration the render gate runs on.
///
/// The built-in one by default, so the gate is reproducible on every
/// machine; an explicit file when the user asks for it. An explicit file
/// is **sanitized** first — the render must not divide by a zero the
/// repair pass would have clamped — and the repairs it triggered are
/// printed, so the user sees why their file renders differently
/// (audit 2026-10-23 §F-124).
fn selftest_config(config: Option<&std::path::Path>) -> Result<agnosis_core::config::Config, i32> {
    let Some(path) = config else {
        return Ok(agnosis_core::config::Config::default());
    };
    let parsed = agnosis_core::config::Config::load_unsanitized_from(path).map_err(|e| {
        eprintln!("agnosis: config error: {e}");
        1
    })?;
    let (cfg, repairs) = cfg_sanitized_for_selftest(parsed);
    report_repairs(&repairs);
    Ok(cfg)
}

/// The sanitized config and its repairs, with a read-only existence
/// predicate: a headless gate must not walk the user's asset folders.
fn cfg_sanitized_for_selftest(
    parsed: agnosis_core::config::Config,
) -> (
    agnosis_core::config::Config,
    Vec<agnosis_core::config::Repair>,
) {
    let mut repairs = Vec::new();
    let cfg = parsed.sanitized_with_reporting(|_, _| true, &mut repairs);
    (cfg, repairs)
}

/// `agnosis assign <slot> [name]`: point a parade slot at a drawn asset.
///
/// The config is loaded, mutated and written back; `--clear` removes the
/// assignment instead of setting one.
fn run_assign(slot: &str, name: Option<&str>, clear: bool) -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    match assign_at(&path, slot, name, clear) {
        Ok(message) => {
            println!("agnosis: {message}");
            0
        }
        Err(e) => {
            eprintln!("agnosis: {e}");
            1
        }
    }
}

/// Write one assignment into the config at `path`.
///
/// The path is a parameter, so the tests write into a temp dir and never
/// touch the user's real config file.
fn assign_at(
    path: &std::path::Path,
    slot: &str,
    name: Option<&str>,
    clear: bool,
) -> Result<String, String> {
    let parsed = agnosis_core::config::AssignmentSlot::parse(slot).map_err(|e| e.to_string())?;
    let value = assignment_value(parsed, name, clear)?;
    let message = match &value {
        Some(name) => format!("{} = {name}", parsed.key()),
        None => format!("{} cleared", parsed.key()),
    };
    let mut cfg = load_config_at(path)?;
    cfg.assignments.set(parsed, value);
    // Only the `[assignments]` table is rewritten, so the user's comments
    // and unknown keys survive (audit 2026-10-20 §3.9).
    agnosis_core::config_edit::write_assignments(path, &cfg.assignments)
        .map_err(|e| e.to_string())?;
    Ok(message)
}

/// The value one `assign` writes: `None` clears the slot, `Some(name)`
/// points it at a drawn asset.
fn assignment_value(
    slot: agnosis_core::config::AssignmentSlot,
    name: Option<&str>,
    clear: bool,
) -> Result<Option<String>, String> {
    if clear {
        return Ok(None);
    }
    let Some(name) = name else {
        return Err(format!(
            "{} needs an asset name (or --clear to remove the assignment)",
            slot.key()
        ));
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(format!("{} needs an asset name", slot.key()));
    }
    agnosis_core::drawn::validate_name(trimmed).map_err(|e| e.to_string())?;
    Ok(Some(trimmed.to_string()))
}

/// Load the config at `path`; a missing file yields the defaults.
///
/// The config is read **unsanitized**: the CLI edits the file the user
/// wrote, so a repair pass must not drop the assignments whose asset does
/// not exist (yet) or rewrite unrelated keys.
fn load_config_at(path: &std::path::Path) -> Result<agnosis_core::config::Config, String> {
    if !path.exists() {
        return Ok(agnosis_core::config::Config::default());
    }
    agnosis_core::config::Config::load_unsanitized_from(path).map_err(|e| e.to_string())
}

/// `agnosis config show` / `agnosis config reset`.
///
/// `show` prints the **effective** configuration — the file after the
/// repair pass, so the user sees the values the app actually uses.
///
/// `reset` writes the default configuration. It is the way back from a
/// config the app refuses to load: a corrupt `config.toml` used to brick
/// every config-reading command with no recovery (audit 2026-10-20 §3.9).
/// The old file is kept as `config.toml.bak`.
fn run_config(command: ConfigCommands) -> i32 {
    match command {
        ConfigCommands::Show => run_config_show(),
        ConfigCommands::Reset { yes } => run_config_reset(yes),
        ConfigCommands::Backup => run_config_backup(),
        ConfigCommands::Validate => run_config_validate(),
        ConfigCommands::Export { file } => run_config_export(&file),
    }
}

/// `agnosis config validate`: parse the file and say what would change.
///
/// The verb the live reload's silence asks for (audit 2026-10-23 §F-097):
/// a rejected edit shows on the parade's input line for three seconds,
/// and this answers the same question with the full detail — the parse
/// error, or every repair the pass would apply. Exit 1 when the file
/// does not parse; 0 when it does (repairs included).
fn run_config_validate() -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    if !path.exists() {
        println!("agnosis: no config file yet — the defaults are in use");
        return 0;
    }
    let (_, repairs) = match agnosis_core::config::Config::load_unsanitized_from(&path) {
        Ok(cfg) => cfg.sanitized_with_repairs(crate::host::drawn_exists),
        Err(e) => {
            eprintln!("agnosis: invalid: {e}");
            eprintln!("agnosis: the parade keeps the last good values");
            return 1;
        }
    };
    report_repairs(&repairs);
    println!("agnosis: config ok ({} repairs)", repairs.len());
    0
}

/// Print one repair per line, in `agnosis config show`'s shape.
fn report_repairs(repairs: &[agnosis_core::config::Repair]) {
    for repair in repairs {
        let key = if repair.section.is_empty() {
            repair.key.to_string()
        } else {
            format!("{}.{}", repair.section, repair.key)
        };
        println!("agnosis: repaired {key} = {}", repair.value);
    }
}

/// The refusal a `agnosis config reset` without `--yes` gets: it says what
/// dies, what survives, and the way through. Exit 2 — the code a script
/// can read as "not confirmed" (audit 2026-10-23 §F-098).
fn refuse_config_reset() -> i32 {
    eprintln!("agnosis: config reset would overwrite every value you changed");
    eprintln!("agnosis: (the colours, the hotkeys, the assignments, the personalities)");
    eprintln!("agnosis: the current file would be kept as config.toml.bak.");
    eprintln!("agnosis: run `agnosis config reset --yes` to go ahead.");
    2
}

/// `agnosis config show`: print the effective configuration.
///
/// The command writes **nothing**: the repairs the pass would apply are
/// reported instead, so the user can see what the app thinks of the file
/// (the audit found the repair invisible, and it repeated forever).
fn run_config_show() -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    let loaded = if path.exists() {
        agnosis_core::config::Config::load_unsanitized_from(&path)
    } else {
        Ok(agnosis_core::config::Config::default())
    };
    let (cfg, repairs) = match loaded {
        Ok(cfg) => cfg.sanitized_with_repairs(crate::host::drawn_exists),
        Err(e) => {
            eprintln!("agnosis: config error: {e}");
            return 1;
        }
    };
    let text = match toml::to_string_pretty(&cfg) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("agnosis: cannot render the config: {e}");
            return 1;
        }
    };
    print!("{text}");
    report_repairs(&repairs);
    0
}

/// `agnosis config backup`: copy the config to a timestamped file.
///
/// Deliberately not the `.bak` (which the writer overwrites on every
/// write): the user wants a *named* history they can keep.
fn run_config_backup() -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    if !path.exists() {
        eprintln!("agnosis: there is no config file to back up");
        return 1;
    }
    let target = match backup_path(&path) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("agnosis: cannot name the backup: {e}");
            return 1;
        }
    };
    match std::fs::copy(&path, &target) {
        Ok(_) => {
            println!("agnosis: config backed up to {}", target.display());
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the backup: {e}");
            1
        }
    }
}

/// The timestamped name of a backup: `config-2026-10-20-1912.toml`.
///
/// The clock is read **once**: the date and the time-of-day come from
/// the same instant, so a backup taken across midnight cannot carry one
/// day's date and the next day's time (audit 2026-10-23 §F-123).
fn backup_path(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let stamp = agnosis_core::stats::date_from_unix_seconds(seconds).replace('-', "");
    let day_seconds = seconds.rem_euclid(86_400);
    let hours = day_seconds / 3600;
    let minutes = (day_seconds % 3600) / 60;
    let name = format!("config-{stamp}-{hours:02}{minutes:02}.toml");
    let parent = path.parent().ok_or("the config file has no folder")?;
    Ok(parent.join(name))
}

/// `agnosis config export`: write the configuration's manifest.
fn run_config_export(file: &std::path::Path) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let manifest = agnosis_core::manifest::Manifest::from_config(&cfg, "agnosis config");
    match agnosis_core::package::export_manifest(file, &manifest) {
        Ok(()) => {
            println!("agnosis: config manifest written to {}", file.display());
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the manifest: {e}");
            1
        }
    }
}

/// `agnosis config reset`: write the default configuration.
///
/// The command overwrites **every** value the user changed, so it is
/// refused until `--yes` names it; the refusal says what is at stake and
/// what is kept (audit 2026-10-22 §UX-02).
fn run_config_reset(yes: bool) -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    if !yes {
        return refuse_config_reset();
    }
    let cfg = agnosis_core::config::Config::default();
    match cfg.save_to(&path) {
        Ok(()) => {
            println!("agnosis: config reset to the defaults ({})", path.display());
            println!("agnosis: the previous file is kept as config.toml.bak");
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the config: {e}");
            1
        }
    }
}

/// `agnosis export`: write a shareable file.
///
/// The **extension** picks the format: `.html` writes the self-contained
/// page, `.gif` writes the animated GIF, and every other extension
/// writes the package (the drawn art, the assignments and the look).
fn run_export(
    file: &std::path::Path,
    fps: u32,
    frames: Option<u32>,
    scale: u32,
    name: Option<&str>,
) -> i32 {
    // A zero fps divides the frame step; a zero scale divides the cell
    // size. The popup's own `--scale` is validated — this verb validates
    // too now, with the same exit code (audit 2026-10-22 §P2-34).
    if fps == 0 {
        eprintln!("agnosis: --fps must be at least 1");
        return 2;
    }
    if scale == 0 {
        eprintln!("agnosis: --scale must be at least 1");
        return 2;
    }
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    // The **rendered** exports need **absolute** picture paths: the name of
    // a backdrop is a platform question, so the shell answers it here.
    // The *package* keeps the names as the user wrote them — a shared
    // file must never carry this machine's paths.
    let mut rendered = cfg.clone();
    crate::host::resolve_backdrop_paths(&mut rendered);
    match agnosis_core::export::Format::for_path(file) {
        Some(agnosis_core::export::Format::Html) => {
            run_export_html(file, &rendered, fps, frames, name)
        }
        Some(agnosis_core::export::Format::Gif) => {
            run_export_gif(file, &rendered, fps, frames, scale)
        }
        None => run_export_package(file, &cfg, name),
    }
}

/// The name an export carries when the user did not give one.
const DEFAULT_EXPORT_NAME: &str = "agnosis package";

/// `agnosis export <file>.html`: the self-contained page.
fn run_export_html(
    file: &std::path::Path,
    cfg: &agnosis_core::config::Config,
    fps: u32,
    frames: Option<u32>,
    name: Option<&str>,
) -> i32 {
    let count = export_frame_count(cfg, fps, frames);
    let name = name.unwrap_or(agnosis_core::export::DEFAULT_NAME);
    match agnosis_core::export::write_html(
        file,
        cfg,
        name,
        count,
        fps,
        &crate::host::asset_dirs(cfg),
    ) {
        Ok(bytes) => {
            println!(
                "agnosis: page written to {} ({} frames, {bytes} bytes)",
                file.display(),
                count
            );
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the page: {e}");
            1
        }
    }
}

/// `agnosis export <file>.gif`: the animated GIF.
fn run_export_gif(
    file: &std::path::Path,
    cfg: &agnosis_core::config::Config,
    fps: u32,
    frames: Option<u32>,
    scale: u32,
) -> i32 {
    let count = export_frame_count(cfg, fps, frames);
    match agnosis_core::export::write_gif(
        file,
        cfg,
        count,
        fps,
        scale,
        &crate::host::asset_dirs(cfg),
    ) {
        Ok(bytes) => {
            println!(
                "agnosis: gif written to {} ({} frames, {bytes} bytes)",
                file.display(),
                count
            );
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the gif: {e}");
            1
        }
    }
}

/// `agnosis export <file>`: the package.
fn run_export_package(
    file: &std::path::Path,
    cfg: &agnosis_core::config::Config,
    name: Option<&str>,
) -> i32 {
    // The core never resolves a platform location: the shell hands over
    // the folders the package is written from.
    let targets = match crate::host::target_dirs() {
        Ok(targets) => targets,
        Err(e) => {
            eprintln!("agnosis: cannot locate the target folders: {e}");
            return 1;
        }
    };
    let name = name.unwrap_or(DEFAULT_EXPORT_NAME);
    match agnosis_core::package::export_all(file, cfg, name, &targets) {
        Ok(()) => {
            println!("agnosis: package written to {}", file.display());
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the package: {e}");
            1
        }
    }
}

/// The frames a recording holds: one loop of the parade.
fn export_frame_count(cfg: &agnosis_core::config::Config, fps: u32, frames: Option<u32>) -> u32 {
    frames.unwrap_or_else(|| {
        let loop_frames = (cfg.loop_seconds * fps as f32).round();
        (loop_frames.max(1.0) as u32).min(agnosis_core::export::frames::MAX_FRAMES)
    })
}

/// `agnosis import`: import a package into this machine's ducks folders.
///
/// The merge policy is the careful part: an asset whose name already
/// exists is **refused** (a silent overwrite is how a user's work is
/// lost) and the user's own assignments always win.
///
/// The **look** is applied too (audit 2026-10-23 §F-045): the package's
/// `[sky]`/`[weather]`/`[message]`/`[grass]` tables land in the user's
/// `config.toml` surgically, in one atomic write — the README's promise
/// that an import reads "the drawn art, the assignments and the look"
/// back is now true.
fn run_import(file: &std::path::Path) -> i32 {
    let targets = match crate::host::target_dirs() {
        Ok(targets) => targets,
        Err(e) => {
            eprintln!("agnosis: cannot locate the ducks folders: {e}");
            return 1;
        }
    };
    let report = match agnosis_core::package::import_from(file, &targets) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("agnosis: could not import the package: {e}");
            return 1;
        }
    };
    print!("{}", import_report_text(&report));
    apply_imported_look(file)
}

/// Render the import report at the CLI edge.
///
/// The core keeps the report as **data** — the fields are `pub` and
/// `summary()` is test-only there — because a user-facing string is the
/// edge's job (audit 2026-10-23 §F-063).
fn import_report_text(report: &agnosis_core::package::ImportReport) -> String {
    let mut out = String::new();
    push_report_count(&mut out, "IMPORTED", &report.imported);
    push_report_count(&mut out, "SKIPPED", &report.skipped);
    out.push_str(&format!("ASSIGNMENTS {}\n", report.merged_assignments));
    push_report_count(&mut out, "CONFLICTS", &report.conflicts);
    out
}

/// One report section: the count line and each entry, indented.
fn push_report_count(out: &mut String, title: &str, entries: &[String]) {
    out.push_str(&format!("{} {}\n", title, entries.len()));
    for entry in entries {
        out.push_str("  ");
        out.push_str(entry);
        out.push('\n');
    }
}

/// Apply the look a package carries to the user's config file.
///
/// The manifest is read **again** from the package: `import_from` owns
/// the asset half, and this owns the look half, through the core's
/// surgical one-write API (contract B→D, audit 2026-10-23 §F-045).
fn apply_imported_look(file: &std::path::Path) -> i32 {
    let applied = read_package_look(file).and_then(|look| match &look {
        None => Ok(()),
        Some(fragment) => config_file().and_then(|path| {
            agnosis_core::package::apply_look_from_fragment(&path, fragment)
                .map_err(|e| e.to_string())
        }),
    });
    match applied {
        Ok(()) => {
            println!("agnosis: look applied");
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not apply the look: {e}");
            1
        }
    }
}

/// The look fragment a package's manifest carries, if any.
///
/// `None` when the package carries no fragment at all (an art-only
/// package): there is nothing to apply and that is not an error. An
/// unreadable or unparseable package is an error — `import_from` would
/// have refused it already, so the two halves cannot disagree silently.
fn read_package_look(
    file: &std::path::Path,
) -> Result<Option<agnosis_core::config::Config>, String> {
    let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
    let archive = agnosis_core::package::ZipArchive::parse(bytes).map_err(|e| e.to_string())?;
    let manifest = archive
        .read(agnosis_core::manifest::MANIFEST_ENTRY)
        .map_err(|e| e.to_string())?;
    let text =
        std::str::from_utf8(manifest).map_err(|_| "the manifest is not UTF-8".to_string())?;
    let manifest = agnosis_core::manifest::Manifest::from_json(text).map_err(|e| e.to_string())?;
    if manifest
        .config_fragment
        .as_table()
        .is_none_or(|t| t.is_empty())
    {
        return Ok(None);
    }
    manifest.config().map(Some).map_err(|e| e.to_string())
}

/// The user's config file path, as a string error.
fn config_file() -> Result<std::path::PathBuf, String> {
    crate::paths::config_file().map_err(|e| e.to_string())
}

/// `agnosis preset`: save, list and apply the scene presets.
fn run_preset(command: PresetCommands) -> i32 {
    let dir = match crate::paths::presets_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("agnosis: cannot locate the presets folder: {e}");
            return 1;
        }
    };
    match command {
        PresetCommands::Save { name } => {
            let cfg = match load_config() {
                Ok(cfg) => cfg,
                Err(code) => return code,
            };
            match agnosis_core::preset::save_preset(&dir, &name, &cfg) {
                Ok(path) => {
                    println!("agnosis: preset saved to {}", path.display());
                    0
                }
                Err(e) => {
                    eprintln!("agnosis: could not save the preset: {e}");
                    1
                }
            }
        }
        PresetCommands::List => {
            let names = agnosis_core::preset::list_presets(&dir);
            if names.is_empty() {
                println!("agnosis: no presets yet");
                return 0;
            }
            for name in names {
                println!("  {name}");
            }
            0
        }
        PresetCommands::Use { name } => {
            let path = dir.join(format!("{name}.{}", agnosis_core::preset::PRESET_EXTENSION));
            let preset = match agnosis_core::preset::load_preset(&path) {
                Ok(preset) => preset,
                Err(e) => {
                    eprintln!("agnosis: could not read the preset: {e}");
                    return 1;
                }
            };
            let config_path = match crate::paths::config_file() {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("agnosis: cannot locate the config file: {e}");
                    return 1;
                }
            };
            match agnosis_core::preset::apply_preset_to_file(&config_path, &preset) {
                Ok(()) => {
                    println!("agnosis: preset {name} applied");
                    0
                }
                Err(e) => {
                    eprintln!("agnosis: could not apply the preset: {e}");
                    1
                }
            }
        }
    }
}

/// `agnosis track`: list, show and choose the MIDI note tracks.
///
/// A track is a **file** in `<config>/tracks/` — a sparse map of step
/// index to note, written by the editor's strip mode. The parade plays
/// the track the config names, and the sequencer's clock is the scene's
/// own clock, so a note fires on the sprite step it sits on.
fn run_track(command: TrackCommands) -> i32 {
    let dir = match crate::paths::tracks_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("agnosis: cannot locate the tracks folder: {e}");
            return 1;
        }
    };
    match command {
        TrackCommands::List => {
            let names = crate::audio::sequencer::list_tracks(&dir);
            if names.is_empty() {
                println!("agnosis: no tracks yet");
                return 0;
            }
            for name in names {
                println!("  {name}");
            }
            0
        }
        TrackCommands::Show { name } => {
            let track = match crate::audio::sequencer::load_track(&dir, &name) {
                Ok(track) => track,
                Err(e) => {
                    eprintln!("agnosis: could not read the track {name}: {e}");
                    return 1;
                }
            };
            if track.is_empty() {
                println!("agnosis: the track {name} has no notes");
                return 0;
            }
            for (step, note) in &track {
                println!(
                    "  step {step:>4}  pitch {:>3}  {} step(s)  velocity {}",
                    note.pitch, note.duration_steps, note.velocity
                );
            }
            0
        }
        TrackCommands::Use { name } => {
            if crate::audio::sequencer::validate_name(&name).is_err() {
                eprintln!("agnosis: {name:?} is not a usable track name");
                return 1;
            }
            if crate::audio::sequencer::load_track(&dir, &name).is_err() {
                eprintln!("agnosis: there is no track called {name}");
                return 1;
            }
            let path = match config_path() {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("agnosis: cannot locate the config file: {e}");
                    return 1;
                }
            };
            match write_the_track_key(&path, &name) {
                Ok(()) => {
                    println!("agnosis: the parade plays {name}");
                    0
                }
                Err(e) => {
                    eprintln!("agnosis: could not point the parade at {name}: {e}");
                    1
                }
            }
        }
        TrackCommands::Clear => {
            let path = match config_path() {
                Ok(path) => path,
                Err(e) => {
                    eprintln!("agnosis: cannot locate the config file: {e}");
                    return 1;
                }
            };
            match write_the_track_key(&path, "") {
                Ok(()) => {
                    println!("agnosis: the parade plays no track");
                    0
                }
                Err(e) => {
                    eprintln!("agnosis: could not clear the track: {e}");
                    1
                }
            }
        }
    }
}

/// The path of the user's config file.
fn config_path() -> Result<std::path::PathBuf, String> {
    crate::paths::config_file().map_err(|e| e.to_string())
}

/// Write the `[audio] track` key into the user's config.
///
/// The write is **surgical**: only that one key is replaced, so the
/// comments, the unknown keys and the other tables survive.
fn write_the_track_key(path: &std::path::Path, name: &str) -> Result<(), String> {
    let value = toml::Value::String(name.to_string()).to_string();
    agnosis_core::config_edit::write_key(path, "audio", "track", &value).map_err(|e| e.to_string())
}

/// `agnosis shot`: write a frame of the parade to a PNG file.
///
/// The popup is a **separate process**, so the command cannot reach the
/// live frame: it renders the current look with the same renderer the
/// selftest uses. The popup's own key copies the *live* frame to the
/// clipboard.
fn run_shot(file: &std::path::Path) -> i32 {
    let mut cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    // The core reads **absolute** picture paths: the name of a backdrop
    // is a platform question, so the shell answers it here.
    crate::host::resolve_backdrop_paths(&mut cfg);
    let (cols, rows, cell_w, cell_h) = cfg.grid_for_monitor(1920, 1080, 1.0);
    let mut font = match agnosis_core::font::Font::new(cell_w, cell_h) {
        Ok(font) => font,
        Err(e) => {
            eprintln!("agnosis: cannot build the font: {e}");
            return 1;
        }
    };
    let mut scene = agnosis_core::scene::Scene::new_with_assets(
        &cfg,
        cols,
        rows,
        cell_w,
        cell_h,
        &crate::host::asset_dirs(&cfg),
    );
    crate::backdrop_loader::load_into(&mut scene, &cfg);
    scene.advance(1.0 / 30.0);
    let mut fb = agnosis_core::blit::FrameBuffer::new(cols as u32 * cell_w, rows as u32 * cell_h);
    agnosis_core::selftest::blit_scene_into(&scene, &mut font, &mut fb);
    match agnosis_core::blit::save_png(&fb, file) {
        Ok(()) => {
            println!("agnosis: shot written to {}", file.display());
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the shot: {e}");
            1
        }
    }
}

/// `agnosis grass`: use a drawn grass tile for the ground band.
///
/// The table is written **surgically**, so the user's comments and the
/// keys the app does not know survive.
fn run_grass(name: Option<&str>, clear: bool) -> i32 {
    let path = match crate::paths::config_file() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("agnosis: cannot locate the config file: {e}");
            return 1;
        }
    };
    grass_at(&path, name, clear)
}

/// `agnosis grass` against a **given** config file.
///
/// The path is a parameter, so the tests write into a temp dir and never
/// touch the user's real config file.
fn grass_at(path: &std::path::Path, name: Option<&str>, clear: bool) -> i32 {
    let action = match grass_action(name, clear) {
        Ok(action) => action,
        Err(code) => return code,
    };
    match action {
        // The bare form is a **query**: it writes nothing at all.
        GrassAction::Show => {
            show_grass_tile(path);
            0
        }
        GrassAction::Set(value) => write_grass_tile(path, Some(value)),
        GrassAction::Clear => write_grass_tile(path, None),
    }
}

/// What `agnosis grass` should do with the `[grass] tile` key.
#[derive(Debug, PartialEq, Eq)]
enum GrassAction {
    /// Print the tile in use; write **nothing**.
    Show,
    /// Point the key at a drawn grass tile.
    Set(String),
    /// Remove the key.
    Clear,
}

/// Decide what `agnosis grass` should do.
///
/// The bare form (no name, no `--clear`) is a **query**, and a query
/// writes nothing. It used to answer "no value" and fall through to the
/// write path, and the write dropped a key whose value was empty — so
/// asking which tile was in use **deleted** it (audit 2026-10-21 §G1).
fn grass_action(name: Option<&str>, clear: bool) -> Result<GrassAction, i32> {
    if clear {
        return Ok(GrassAction::Clear);
    }
    let Some(name) = name else {
        return Ok(GrassAction::Show);
    };
    let trimmed = name.trim();
    if let Err(e) = agnosis_core::drawn::validate_name(trimmed) {
        eprintln!("agnosis: {e}");
        return Err(1);
    }
    let dir = match crate::paths::drawn_grass_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("agnosis: cannot locate the drawn folder: {e}");
            return Err(1);
        }
    };
    if let Err(e) =
        agnosis_core::drawn::load_from(&dir, agnosis_core::drawn::DrawnKind::Grass, trimmed)
    {
        eprintln!("agnosis: {e}");
        return Err(1);
    }
    Ok(GrassAction::Set(
        toml::Value::String(trimmed.to_string()).to_string(),
    ))
}

/// Print the grass tile in use — and write nothing.
fn show_grass_tile(path: &std::path::Path) {
    let cfg = match load_config_at(path) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("agnosis: config error: {e}");
            return;
        }
    };
    match cfg.grass.tile {
        Some(tile) => println!("agnosis: grass tile = {tile}"),
        None => println!("agnosis: no grass tile (the built-in band is in use)"),
    }
}

/// Write the `[grass] tile` key — or remove it when the value is `None`.
fn write_grass_tile(path: &std::path::Path, value: Option<String>) -> i32 {
    let entries = [("tile", value)];
    match agnosis_core::config_edit::write_table(path, "grass", &entries) {
        Ok(()) => {
            match &entries[0].1 {
                Some(name) => println!("agnosis: grass tile = {name}"),
                None => println!("agnosis: grass tile cleared"),
            }
            0
        }
        Err(e) => {
            eprintln!("agnosis: could not write the grass tile: {e}");
            1
        }
    }
}

/// `agnosis list-drawn`: print the drawn sprite and balloon names.
fn run_list_drawn() -> i32 {
    let sprites = crate::paths::drawn_sprites_dir();
    let balloons = crate::paths::drawn_balloons_dir();
    let decorations = crate::paths::drawn_decorations_dir();
    let (sprites, balloons, decorations) = match (sprites, balloons, decorations) {
        (Ok(sprites), Ok(balloons), Ok(decorations)) => (sprites, balloons, decorations),
        (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => {
            eprintln!("agnosis: cannot resolve the drawn-asset folders: {e}");
            return 1;
        }
    };
    print!("{}", list_drawn_in(&sprites, &balloons, &decorations));
    0
}

/// `agnosis menubar`: the macOS menu-bar trigger.
///
/// The call blocks: the app owns the main thread and its run loop. On
/// every other platform the backend answers "the menu bar is macOS
/// only", which is reported here instead of a panic.
fn run_menubar() -> i32 {
    match crate::platform::menubar::run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agnosis: menubar failed: {e}");
            1
        }
    }
}

/// The `list-drawn` report for two explicit folders, so the test can point
/// it at temp dirs instead of the user's asset folders.
fn list_drawn_in(
    sprite_dir: &std::path::Path,
    balloon_dir: &std::path::Path,
    decoration_dir: &std::path::Path,
) -> String {
    let sprites = agnosis_core::drawn::list_in(sprite_dir);
    let balloons = agnosis_core::drawn::list_in(balloon_dir);
    let decorations = agnosis_core::drawn::list_in(decoration_dir);
    let mut out = String::new();
    push_drawn_section(&mut out, "SPRITES", &sprites);
    push_drawn_section(&mut out, "BALLOONS", &balloons);
    push_drawn_section(&mut out, "DECORATIONS", &decorations);
    out
}

/// Append one `list-drawn` section (`SPRITES` / `BALLOONS`).
fn push_drawn_section(out: &mut String, title: &str, names: &[String]) {
    out.push_str(title);
    out.push_str(":\n");
    if names.is_empty() {
        out.push_str("  (none)\n");
        return;
    }
    for name in names {
        out.push_str("  ");
        out.push_str(name);
        out.push('\n');
    }
}

/// Hidden `popup` subcommand: the daemon spawn target.
fn run_popup(origin: (i32, i32), size: (u32, u32), scale: f64) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    if !(scale.is_finite() && scale > 0.0) {
        eprintln!("agnosis: bad --scale {scale}");
        return 2;
    }
    if size.0 == 0 || size.1 == 0 {
        eprintln!("agnosis: bad --size {},{}", size.0, size.1);
        return 2;
    }
    let args = crate::popup::PopupArgs {
        origin,
        size,
        scale,
    };
    match crate::popup::run(&cfg, &args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agnosis: popup failed: {e}");
            1
        }
    }
}

/// Hidden `editor` subcommand: the daemon spawn target for the
/// draw-your-own editor.
///
/// `size` is the **monitor** size: the editor computes its own window rect
/// from it (80 % of the monitor, min 900 × 600, max 1920 × 1200, centred),
/// so the daemon only has to hand over the monitor geometry.
fn run_editor(origin: (i32, i32), size: (u32, u32)) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    if size.0 == 0 || size.1 == 0 {
        eprintln!("agnosis: bad --size {},{}", size.0, size.1);
        return 2;
    }
    let args = crate::editor::EditorArgs { origin, size };
    match crate::editor::run(&cfg, &args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agnosis: editor failed: {e}");
            1
        }
    }
}

/// `agnosis help`: the in-app HELP window (roadmap HELP_WINDOW §1).
///
/// The README is read **here**, at open time, before the window exists:
/// the on-disk file wins and the build's embedded copy is the fallback,
/// so a bare exe shows the documentation it shipped with. The window
/// opens on the spawner's monitor when the geometry rides along (the
/// parade's F1 passes its own), on the configured monitor otherwise —
/// at 80 % of it, clamped and centred.
fn run_help(origin: Option<(i32, i32)>, size: Option<(u32, u32)>) -> i32 {
    // Self-heal (audit 2026-10-25) BEFORE the read: when nothing on disk
    // answers, the embedded copy goes back beside the binary first, so the
    // very first open reads a real file — best effort, and a live file is
    // never touched.
    crate::host::restore_readme_if_missing();
    let text = crate::host::readme_text();
    let (Some(origin), Some(size)) = (origin, size) else {
        // A half pair falls back to the config too: the window is
        // centred either way, so a partial hint would only move it to a
        // monitor nobody asked for.
        return run_help_on_config_monitor(&text);
    };
    run_help_window(&text, crate::help::HelpArgs { origin, size })
}

/// The F1 path: the parade's monitor rides along, no config read.
fn run_help_window(text: &str, args: crate::help::HelpArgs) -> i32 {
    match crate::help::run(text, &args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("agnosis: help failed: {e}");
            1
        }
    }
}

/// The bare `agnosis help` path: resolve the configured monitor, then open.
fn run_help_on_config_monitor(text: &str) -> i32 {
    let cfg = match load_config() {
        Ok(cfg) => cfg,
        Err(code) => return code,
    };
    let args = match monitor::resolve(&cfg) {
        Ok(target) => crate::help::HelpArgs {
            origin: target.origin,
            size: target.size,
        },
        Err(e) => {
            eprintln!("agnosis: no monitor for the help window: {e}");
            return 1;
        }
    };
    run_help_window(text, args)
}

/// Conversion radii for `add` from the resolved popup monitor (audit §P0-3):
/// the same `grid_for_monitor` + `balloon_radii_cells` helpers the live
/// scene uses. Warns and falls back to the historical 1920x1080 only when no
/// monitor resolves.
fn live_add_radii(cfg: &agnosis_core::config::Config) -> (i64, i64) {
    match monitor::resolve(cfg) {
        Ok(target) => radii_for(cfg, target.size.0, target.size.1),
        Err(e) => {
            agnosis_core::trace::warn(&format!(
                "cli.add no monitor resolved ({e}), assuming 1920x1080"
            ));
            radii_for(cfg, 1920, 1080)
        }
    }
}

/// Radii from an explicit monitor size: the single geometry both `add` paths
/// share with the live scene.
fn radii_for(cfg: &agnosis_core::config::Config, w: u32, h: u32) -> (i64, i64) {
    let (_, _, cell_w, cell_h) = cfg.grid_for_monitor(w, h, 1.0);
    let (rx, ry) = agnosis_core::balloon::balloon_radii_cells(
        cfg.balloons.balloon_radius_rows,
        cell_w,
        cell_h,
    );
    (rx.round() as i64, ry.round() as i64)
}

/// Persist a sprite as the `<stem>.sprite.json` sibling of the source art,
/// stamped with the conversion radii (audit §P0-3).
///
/// The cache is a *sibling* of the art, because that is the only place the
/// watcher looks for it: it loads `<stem>.sprite.json` next to the source
/// file. A cache written to the balloon-folder root instead would be
/// invisible for art in a subfolder, and for art from anywhere else.
fn cache_sprite(
    source: &std::path::Path,
    sprite: &agnosis_core::balloon::AsciiSprite,
    radii: (i64, i64),
) -> Result<PathBuf, String> {
    let dir = crate::paths::balloon_dir().map_err(|e| e.to_string())?;
    cache_sprite_in(&dir, source, sprite, radii)
}

/// [`cache_sprite`] with an explicit balloon folder, so the placement rules
/// can be tested without touching the user's real folder.
fn cache_sprite_in(
    dir: &std::path::Path,
    source: &std::path::Path,
    sprite: &agnosis_core::balloon::AsciiSprite,
    radii: (i64, i64),
) -> Result<PathBuf, String> {
    let installed = install_source_into(dir, source)?;
    let path = crate::balloon_resolve::sibling_sprite(&installed)
        .ok_or_else(|| format!("no file stem in {}", source.display()))?;
    agnosis_core::image_ascii::save_sprite_stamped(sprite, &path, radii.0, radii.1)
        .map_err(|e| e.to_string())?;
    Ok(path)
}

/// Make the source art visible to the watcher and return the path the
/// watcher will scan.
///
/// Art that already lives in the balloon folder (the watcher scans it
/// recursively) stays where it is. Art from anywhere else is copied into the
/// folder, so `agnosis add` really does add a balloon — the README promises
/// the picture ends up in the balloons folder.
fn install_source_into(dir: &std::path::Path, source: &std::path::Path) -> Result<PathBuf, String> {
    if is_inside(dir, source) {
        return Ok(source.to_path_buf());
    }
    let name = source
        .file_name()
        .ok_or_else(|| format!("no file name in {}", source.display()))?;
    let target = free_target(dir, name, source)?;
    std::fs::copy(source, &target).map_err(|e| format!("cannot copy {}: {e}", source.display()))?;
    Ok(target)
}

/// A free path in `dir` for `name`: the name itself when it is free or holds
/// the same bytes as `source`, else the first free `<stem>-N<ext>`.
///
/// An existing balloon is never overwritten: `agnosis add` must not destroy
/// art the user already has.
fn free_target(
    dir: &std::path::Path,
    name: &std::ffi::OsStr,
    source: &std::path::Path,
) -> Result<PathBuf, String> {
    let first = dir.join(name);
    if !first.exists() || same_bytes(&first, source) {
        return Ok(first);
    }
    let path = PathBuf::from(name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("no file stem in {}", name.to_string_lossy()))?;
    let extension = path.extension().and_then(|e| e.to_str());
    for index in 2..=999 {
        let candidate = match extension {
            Some(ext) => dir.join(format!("{stem}-{index}.{ext}")),
            None => dir.join(format!("{stem}-{index}")),
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(format!("no free name for {}", name.to_string_lossy()))
}

/// True when `path` lives inside `dir`.
fn is_inside(dir: &std::path::Path, path: &std::path::Path) -> bool {
    let (Ok(dir), Ok(path)) = (dir.canonicalize(), path.canonicalize()) else {
        return false;
    };
    path.starts_with(dir)
}

/// True when two files have the same length and the same content.
fn same_bytes(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(a_meta), Ok(b_meta)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if a_meta.len() != b_meta.len() {
        return false;
    }
    let (Ok(a_bytes), Ok(b_bytes)) = (std::fs::read(a), std::fs::read(b)) else {
        return false;
    };
    a_bytes == b_bytes
}

/// True when a daemon stop endpoint is already armed and reachable.
///
/// The probe itself lives in [`crate::platform::daemon_ctl`] so the
/// popup's onboarding hint asks the very same question the `start` verb
/// does — the two cannot drift apart.
fn daemon_running() -> bool {
    crate::platform::daemon_ctl::endpoint_armed()
}

/// Parse an `X,Y` pair of signed integers.
fn parse_pair_i32(raw: &str) -> Result<(i32, i32), String> {
    let (a, b) = split_pair(raw)?;
    let x: i32 = a.parse().map_err(|_| format!("bad --origin {raw:?}"))?;
    let y: i32 = b.parse().map_err(|_| format!("bad --origin {raw:?}"))?;
    Ok((x, y))
}

/// Parse a `W,H` pair of unsigned integers.
fn parse_pair_u32(raw: &str) -> Result<(u32, u32), String> {
    let (a, b) = split_pair(raw)?;
    let w: u32 = a.parse().map_err(|_| format!("bad --size {raw:?}"))?;
    let h: u32 = b.parse().map_err(|_| format!("bad --size {raw:?}"))?;
    if w == 0 || h == 0 {
        return Err(format!("bad --size {raw:?}"));
    }
    Ok((w, h))
}

/// Split one `A,B` pair, rejecting missing or extra components.
fn split_pair(raw: &str) -> Result<(&str, &str), String> {
    let mut parts = raw.split(',');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(a), Some(b), None) => Ok((a.trim(), b.trim())),
        _ => Err(format!("expected `X,Y`, got {raw:?}")),
    }
}

/// Monitor enumeration and cursor/monitor resolution.
///
/// Every platform answers the same two questions: which displays exist
/// (in physical pixels) and where the pointer is. The per-OS backends live
/// in `imp`:
///
/// | Platform | Displays | Pointer |
/// | --- | --- | --- |
/// | Windows | `EnumDisplayMonitors` + `GetDpiForMonitor` | `GetCursorPos` |
/// | Linux | XRandR CRTCs | X11 `QueryPointer` |
/// | macOS | `NSScreen` frames | `NSEvent::mouseLocation` |
///
/// The non-Windows probes fall back to the historical 1920x1080@1x
/// assumption — with a warn trace, so the fallback stays visible — when the
/// platform cannot answer (no X server, no objc runtime, XRandR too old).
mod monitor {
    /// One display in physical pixels plus its scale and primary flag.
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub(super) struct Display {
        pub(super) origin: (i32, i32),
        pub(super) size: (u32, u32),
        pub(super) scale: f64,
        pub(super) primary: bool,
    }

    /// Resolve the configured monitor into popup geometry.
    pub(super) fn resolve(
        cfg: &agnosis_core::config::Config,
    ) -> Result<crate::popup::PopupArgs, String> {
        let displays = enumerate().map_err(|e| format!("monitor query failed: {e}"))?;
        if displays.iter().all(|d| d.size.0 == 0 || d.size.1 == 0) {
            return Err("no usable monitor found".to_string());
        }
        pick(cfg, &displays, cursor_point()).ok_or_else(|| "no usable monitor found".to_string())
    }

    /// The chosen display's geometry, given a display list and a pointer.
    ///
    /// One implementation of the choice (audit 2026-10-23 §F-125): the
    /// live resolve and the tests — which cannot ask a platform where
    /// the pointer is — share it, so the test can no longer drift from
    /// the code it pins. `cursor` is `None` when the platform cannot
    /// report the pointer; the primary display answers instead.
    fn pick(
        cfg: &agnosis_core::config::Config,
        displays: &[Display],
        cursor: Option<(i32, i32)>,
    ) -> Option<crate::popup::PopupArgs> {
        let fallback = primary(displays).or_else(|| displays.first().copied())?;
        let picked = match cfg.monitor {
            agnosis_core::config::MonitorChoice::Primary => fallback,
            agnosis_core::config::MonitorChoice::Cursor => cursor
                .and_then(|point| display_under(displays, point))
                .unwrap_or(fallback),
        };
        Some(crate::popup::PopupArgs {
            origin: picked.origin,
            size: picked.size,
            scale: picked.scale,
        })
    }

    /// Primary display, if any has the flag set.
    fn primary(displays: &[Display]) -> Option<Display> {
        displays.iter().copied().find(|d| d.primary)
    }

    /// The display under the pointer, or the nearest one to it.
    fn display_under(displays: &[Display], point: (i32, i32)) -> Option<Display> {
        displays
            .iter()
            .copied()
            .find(|d| contains(d, point))
            .or_else(|| nearest_list(displays, point))
    }

    /// True when `point` lies inside the display rect.
    fn contains(d: &Display, point: (i32, i32)) -> bool {
        let w = d.size.0 as i64;
        let h = d.size.1 as i64;
        let x = point.0 as i64;
        let y = point.1 as i64;
        x >= d.origin.0 as i64
            && x < d.origin.0 as i64 + w
            && y >= d.origin.1 as i64
            && y < d.origin.1 as i64 + h
    }

    /// Nearest display to the pointer over the full list.
    fn nearest_list(displays: &[Display], point: (i32, i32)) -> Option<Display> {
        displays.iter().copied().min_by_key(|d| {
            let cx = d.origin.0 as i64 + d.size.0 as i64 / 2;
            let cy = d.origin.1 as i64 + d.size.1 as i64 / 2;
            let dx = point.0 as i64 - cx;
            let dy = point.1 as i64 - cy;
            dx * dx + dy * dy
        })
    }

    /// Enumerate the displays of the current session, in physical pixels.
    pub(super) fn enumerate() -> Result<Vec<Display>, String> {
        imp::enumerate()
    }

    /// Pointer position in physical pixels, if the platform can report it.
    ///
    /// `None` means "unknown": the caller then picks the primary display
    /// instead of guessing the top-left corner of the screen.
    fn cursor_point() -> Option<(i32, i32)> {
        imp::cursor_point()
    }

    #[cfg(not(target_os = "windows"))]
    /// The historical single-display assumption, used when a platform probe
    /// cannot answer (no X server, no objc runtime, XRandR too old).
    ///
    /// The caller warns, so the fallback shows up in the trace file.
    fn fallback_displays() -> Vec<Display> {
        vec![Display {
            origin: (0, 0),
            size: (1920, 1080),
            scale: 1.0,
            primary: true,
        }]
    }

    /// Windows: `EnumDisplayMonitors` + `GetDpiForMonitor`.
    ///
    /// The per-monitor DPI is queried while the `HMONITOR` is still alive:
    /// virtualized enumeration is exactly what shrank the popup to
    /// 3072x1728 on a 3840x2160@125% panel.
    #[cfg(target_os = "windows")]
    mod imp {
        use super::Display;

        pub(super) fn enumerate() -> Result<Vec<Display>, String> {
            use windows::Win32::Foundation::{LPARAM, RECT};
            use windows::Win32::Graphics::Gdi::{
                EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
            };
            use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

            struct Sink {
                displays: Vec<Display>,
            }

            unsafe extern "system" fn callback(
                monitor: HMONITOR,
                _dc: HDC,
                _rect: *mut RECT,
                data: LPARAM,
            ) -> windows::core::BOOL {
                let sink = &mut *(data.0 as *mut Sink);
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    rcMonitor: RECT::default(),
                    rcWork: RECT::default(),
                    dwFlags: 0,
                };
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    let w = (info.rcMonitor.right - info.rcMonitor.left).max(1) as u32;
                    let h = (info.rcMonitor.bottom - info.rcMonitor.top).max(1) as u32;
                    // Query this monitor's own DPI while we still hold its
                    // HMONITOR; virtualized enumeration is exactly what shrank
                    // the popup to 3072x1728 on a 3840x2160@125% panel.
                    let scale = dpi_scale_for_monitor(monitor);
                    sink.displays.push(Display {
                        origin: (info.rcMonitor.left, info.rcMonitor.top),
                        size: (w, h),
                        scale,
                        primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                    });
                }
                true.into()
            }

            let mut sink = Sink {
                displays: Vec::new(),
            };
            let ok = unsafe {
                EnumDisplayMonitors(
                    None,
                    None,
                    Some(callback),
                    LPARAM(&mut sink as *mut Sink as isize),
                )
            };
            if !ok.as_bool() {
                return Err("EnumDisplayMonitors failed".to_string());
            }
            if sink.displays.is_empty() {
                return Err("no monitors enumerated".to_string());
            }
            Ok(sink.displays)
        }

        /// Cursor position via `GetCursorPos` at invocation time.
        pub(super) fn cursor_point() -> Option<(i32, i32)> {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
            let mut point = POINT { x: 0, y: 0 };
            unsafe { GetCursorPos(&mut point).ok()? };
            Some((point.x, point.y))
        }

        /// Per-monitor DPI scale for one enumerated `HMONITOR` via
        /// `GetDpiForMonitor(MDT_EFFECTIVE_DPI)`. Falls back to the legacy system
        /// `GetDC(None)` query, then to 1.0, when the per-monitor query fails.
        fn dpi_scale_for_monitor(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> f64 {
            use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
            use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
            unsafe {
                let mut dpi_x = 0u32;
                let mut dpi_y = 0u32;
                if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y).is_ok()
                    && dpi_x > 0
                {
                    return dpi_x as f64 / 96.0;
                }
                let dc = GetDC(None);
                if dc.is_invalid() {
                    return 1.0;
                }
                let dpi = GetDeviceCaps(Some(dc), LOGPIXELSX);
                ReleaseDC(None, dc);
                if dpi > 0 {
                    dpi as f64 / 96.0
                } else {
                    1.0
                }
            }
        }
    }

    /// Linux: the XRandR CRTCs are the monitors.
    ///
    /// Each CRTC carries its pixel origin and pixel size, which is exactly
    /// what the popup geometry needs. X11 is DPI-unaware, so the scale is
    /// derived from the EDID physical size when the server reports one and
    /// stays 1.0 otherwise (audit P1-14).
    #[cfg(target_os = "linux")]
    mod imp {
        use super::Display;
        use x11rb::connection::Connection;
        use x11rb::protocol::randr;
        use x11rb::protocol::xproto;

        /// Reference DPI the scale factor is expressed against.
        const REFERENCE_DPI: f64 = 96.0;

        /// Millimetres per inch, for the EDID physical size.
        const MM_PER_INCH: f64 = 25.4;

        /// Scale bounds: a wrong EDID must not produce a silly factor.
        const MIN_SCALE: f64 = 1.0;
        const MAX_SCALE: f64 = 4.0;

        /// Displays of the X session, one per active CRTC.
        pub(super) fn enumerate() -> Result<Vec<Display>, String> {
            match probe() {
                Ok(displays) => Ok(displays),
                Err(e) => {
                    agnosis_core::trace::warn(&format!(
                        "cli.monitor XRandR probe failed ({e}), assuming 1920x1080@1x"
                    ));
                    Ok(super::fallback_displays())
                }
            }
        }

        /// Pointer position in root (screen) coordinates, in pixels.
        pub(super) fn cursor_point() -> Option<(i32, i32)> {
            let (conn, screen) = x11rb::connect(None).ok()?;
            let root = conn.setup().roots[screen].root;
            let reply = xproto::query_pointer(&conn, root).ok()?.reply().ok()?;
            Some((i32::from(reply.root_x), i32::from(reply.root_y)))
        }

        /// The active CRTCs of the RandR screen, as displays.
        fn probe() -> Result<Vec<Display>, String> {
            let (conn, screen) =
                x11rb::connect(None).map_err(|e| format!("X11 connect failed: {e}"))?;
            let root = conn.setup().roots[screen].root;
            require_randr(&conn)?;
            let crtcs = crtc_ids(&conn, root)?;
            let displays = displays_for(&conn, &crtcs)?;
            if displays.is_empty() {
                return Err("no active CRTC".to_string());
            }
            Ok(displays)
        }

        /// Fail when the X server has no RandR extension: the probe cannot
        /// answer anything without it.
        fn require_randr(conn: &impl Connection) -> Result<(), String> {
            let present = conn
                .extension_information(randr::X11_EXTENSION_NAME)
                .map_err(|e| format!("XRandR query failed: {e}"))?
                .is_some();
            if present {
                return Ok(());
            }
            Err("XRandR extension missing".to_string())
        }

        /// The CRTC ids of the RandR screen.
        fn crtc_ids(
            conn: &impl Connection,
            root: xproto::Window,
        ) -> Result<Vec<randr::Crtc>, String> {
            let resources = randr::get_screen_resources(conn, root)
                .map_err(|e| format!("RandR GetScreenResources failed: {e}"))?
                .reply()
                .map_err(|e| format!("RandR GetScreenResources reply failed: {e}"))?;
            Ok(resources.crtcs)
        }

        /// One display per CRTC. The first CRTC of the screen is the primary
        /// one (RandR keeps the primary first); a CRTC without a mode is a
        /// disabled output and is skipped.
        fn displays_for(
            conn: &impl Connection,
            crtcs: &[randr::Crtc],
        ) -> Result<Vec<Display>, String> {
            let mut displays = Vec::new();
            for (index, crtc) in crtcs.iter().enumerate() {
                let Some(display) = crtc_display(conn, *crtc, index == 0)? else {
                    continue;
                };
                displays.push(display);
            }
            Ok(displays)
        }

        /// One CRTC as a display; `None` when the CRTC has no mode.
        fn crtc_display(
            conn: &impl Connection,
            crtc: randr::Crtc,
            primary: bool,
        ) -> Result<Option<Display>, String> {
            let info = randr::get_crtc_info(conn, crtc, 0)
                .map_err(|e| format!("RandR GetCrtcInfo failed: {e}"))?
                .reply()
                .map_err(|e| format!("RandR GetCrtcInfo reply failed: {e}"))?;
            if info.width == 0 || info.height == 0 {
                return Ok(None);
            }
            let scale = crtc_scale(conn, &info, info.width);
            Ok(Some(Display {
                origin: (i32::from(info.x), i32::from(info.y)),
                size: (u32::from(info.width), u32::from(info.height)),
                scale,
                primary,
            }))
        }

        /// Scale factor of a CRTC from the EDID physical size of its output:
        /// X11 has no per-monitor DPI, so the physical millimetres are the
        /// only hint. Unknown sizes stay at 1.0.
        fn crtc_scale(conn: &impl Connection, info: &randr::GetCrtcInfoReply, width: u16) -> f64 {
            let Some(output) = info.outputs.first() else {
                return 1.0;
            };
            let Ok(cookie) = randr::get_output_info(conn, *output, 0) else {
                return 1.0;
            };
            let Ok(reply) = cookie.reply() else {
                return 1.0;
            };
            dpi_scale(width, reply.mm_width).clamp(MIN_SCALE, MAX_SCALE)
        }

        /// Scale factor from a pixel width and the physical width in
        /// millimetres the EDID reports.
        fn dpi_scale(pixels: u16, millimetres: u32) -> f64 {
            if pixels == 0 || millimetres == 0 {
                return 1.0;
            }
            let inches = f64::from(millimetres) / MM_PER_INCH;
            let dpi = f64::from(pixels) / inches;
            dpi / REFERENCE_DPI
        }
    }

    /// macOS: the `NSScreen` frames are the monitors.
    ///
    /// `NSScreen` reports frames in *points* of the global screen space
    /// (origin: top-left of the main screen, y downwards). The popup works in
    /// physical pixels, so every frame is scaled by the screen's backing
    /// scale factor (audit P1-14).
    #[cfg(target_os = "macos")]
    mod imp {
        use super::Display;
        use objc2::rc::autoreleasepool;
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSEvent, NSScreen};

        /// Scale bounds: a bogus frame must not produce a silly factor.
        const MIN_SCALE: f64 = 1.0;
        const MAX_SCALE: f64 = 4.0;

        /// Displays of the session, one per `NSScreen`.
        pub(super) fn enumerate() -> Result<Vec<Display>, String> {
            match probe() {
                Ok(displays) => Ok(displays),
                Err(e) => {
                    agnosis_core::trace::warn(&format!(
                        "cli.monitor NSScreen probe failed ({e}), assuming 1920x1080@1x"
                    ));
                    Ok(super::fallback_displays())
                }
            }
        }

        /// Pointer position in the top-left screen space, in pixels.
        pub(super) fn cursor_point() -> Option<(i32, i32)> {
            // AppKit is main-thread only, so a probe from the daemon's hotkey
            // thread answers "unknown" and the caller picks the primary
            // display instead of the top-left corner.
            let mtm = MainThreadMarker::new()?;
            autoreleasepool(|_| {
                let point = NSEvent::mouseLocation();
                let main = NSScreen::mainScreen(mtm)?;
                // The scale of the screen **under the pointer**: the main
                // screen's scale was used before, which put the pointer in
                // the wrong place on a mixed-DPI setup (audit 2026-10-20
                // §3.18).
                let scale = scale_under_pointer(mtm, point.x, point.y);
                // `mouseLocation` counts from the bottom-left of the main
                // screen, the `NSScreen` frames from the top-left.
                let x = point.x * scale;
                let y = (main.frame().size.height - point.y) * scale;
                Some((round_i32(x), round_i32(y)))
            })
        }

        /// The backing scale of the screen the pointer is on.
        ///
        /// The main screen's scale when the pointer is nowhere (it always
        /// is on one, but the lookup must not fail).
        fn scale_under_pointer(mtm: MainThreadMarker, x: f64, y: f64) -> f64 {
            let screens = NSScreen::screens(mtm);
            let count = screens.count();
            for index in 0..count {
                let screen = screens.objectAtIndex(index);
                let frame = screen.frame();
                let inside = frame.origin.x <= x
                    && x <= frame.origin.x + frame.size.width
                    && frame.origin.y <= y
                    && y <= frame.origin.y + frame.size.height;
                if inside {
                    return backing_scale(&screen);
                }
            }
            NSScreen::mainScreen(mtm)
                .map(|main| backing_scale(&main))
                .unwrap_or(1.0)
        }

        /// Every screen as a display; the main screen is the primary one.
        fn probe() -> Result<Vec<Display>, String> {
            let mtm =
                MainThreadMarker::new().ok_or_else(|| "not on the main thread".to_string())?;
            autoreleasepool(|_| {
                let screens = NSScreen::screens(mtm);
                let main = NSScreen::mainScreen(mtm);
                let main_height = main.as_ref().map(|m| m.frame().size.height).unwrap_or(0.0);
                let count = screens.count();
                let mut displays = Vec::new();
                for index in 0..count {
                    let screen = screens.objectAtIndex(index);
                    displays.push(display_for(
                        &screen,
                        is_primary(&screen, main.as_deref()),
                        main_height,
                    ));
                }
                Ok(displays)
            })
        }

        /// True when `screen` is the main screen, i.e. the primary display.
        fn is_primary(screen: &NSScreen, main: Option<&NSScreen>) -> bool {
            main.is_some_and(|m| m == screen)
        }

        /// One screen as a display in physical pixels.
        ///
        /// `main_height` is the main screen's height: the origins are
        /// reported in the **top-left** space the pointer is reported in,
        /// so a display above the main one gets a negative y. The Cocoa
        /// frame origin alone would have put the two in different spaces
        /// (audit 2026-10-20 §3.18).
        fn display_for(screen: &NSScreen, primary: bool, main_height: f64) -> Display {
            let frame = screen.frame();
            let scale = backing_scale(screen);
            let top = main_height - (frame.origin.y + frame.size.height);
            Display {
                origin: (round_i32(frame.origin.x * scale), round_i32(top * scale)),
                size: (
                    round_u32(frame.size.width * scale),
                    round_u32(frame.size.height * scale),
                ),
                scale,
                primary,
            }
        }

        /// Backing scale factor of a screen: its backing pixels per point.
        ///
        /// `NSScreen::backingScaleFactor` needs a CoreFoundation feature this
        /// crate does not enable, so the factor is derived from the backing
        /// rect instead.
        fn backing_scale(screen: &NSScreen) -> f64 {
            let frame = screen.frame();
            if frame.size.width <= 0.0 {
                return 1.0;
            }
            let backing = screen.convertRectToBacking(frame);
            (backing.size.width / frame.size.width).clamp(MIN_SCALE, MAX_SCALE)
        }

        /// Round a coordinate to whole pixels.
        fn round_i32(value: f64) -> i32 {
            value.round() as i32
        }

        /// Round a length to whole pixels.
        fn round_u32(value: f64) -> u32 {
            value.round().max(0.0) as u32
        }
    }

    /// Platforms without a monitor probe: the historical assumption, with
    /// the warn trace that made it visible before.
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    mod imp {
        use super::Display;

        pub(super) fn enumerate() -> Result<Vec<Display>, String> {
            agnosis_core::trace::warn(
                "cli.monitor no monitor probe for this platform, assuming 1920x1080@1x",
            );
            Ok(super::fallback_displays())
        }

        /// No pointer probe on this platform: the caller falls back to the
        /// primary display.
        pub(super) fn cursor_point() -> Option<(i32, i32)> {
            None
        }
    }

    #[cfg(test)]
    /// Pick from an injected list with an injected cursor point.
    /// The test's door into [`pick`]: the pointer is a parameter, so the
    /// test drives the same choice the live resolve makes
    /// (audit 2026-10-23 §F-125).
    pub(super) fn pick_for_test(
        cfg: &agnosis_core::config::Config,
        displays: &[Display],
        cursor: (i32, i32),
    ) -> Option<crate::popup::PopupArgs> {
        pick(cfg, displays, Some(cursor))
    }
}

#[cfg(test)]
mod tests {
    use super::monitor::{pick_for_test, Display};
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn displays() -> Vec<Display> {
        vec![
            Display {
                origin: (0, 0),
                size: (1920, 1080),
                scale: 1.0,
                primary: true,
            },
            Display {
                origin: (1920, 0),
                size: (1920, 1080),
                scale: 1.25,
                primary: false,
            },
        ]
    }

    #[test]
    fn selftest_parses_frames() {
        let parsed = parse_args(args(&["agnosis", "selftest", "--frames", "5"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Selftest {
                frames,
                out,
                config: _,
            } => {
                assert_eq!(*frames, 5);
                assert_eq!(out, &PathBuf::from("./selftest_out"));
            }
            other => panic!("expected selftest, got {other:?}"),
        }
    }

    #[test]
    fn selftest_defaults_apply() {
        let parsed = parse_args(args(&["agnosis", "selftest"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Selftest {
                frames,
                out,
                config: _,
            } => {
                assert_eq!(*frames, 30);
                assert_eq!(out, &PathBuf::from("./selftest_out"));
            }
            other => panic!("expected selftest, got {other:?}"),
        }
    }

    /// `agnosis export --name` reaches the runner (audit 2026-10-23 §F-126):
    /// the creation's name is the manifest's, and it used to be
    /// hard-coded.
    #[test]
    fn export_takes_a_name() {
        let parsed = parse_args(args(&[
            "agnosis",
            "export",
            "trip.agnosis",
            "--name",
            "trip",
        ]))
        .expect("parse");
        match parsed.kind() {
            ParsedKind::Export { name, .. } => assert_eq!(name.as_deref(), Some("trip")),
            other => panic!("expected export, got {other:?}"),
        }
        // Without the flag the runner supplies its own default.
        let parsed = parse_args(args(&["agnosis", "export", "trip.agnosis"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Export { name, .. } => assert!(name.is_none()),
            other => panic!("expected export, got {other:?}"),
        }
    }

    /// `agnosis config validate` parses as its own verb
    /// (audit 2026-10-23 §F-097).
    #[test]
    fn config_validate_parses() {
        let parsed = parse_args(args(&["agnosis", "config", "validate"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Config(ConfigCommands::Validate) => {}
            other => panic!("expected config validate, got {other:?}"),
        }
    }

    /// The CLI renders the import report from the **pub fields** — the
    /// core's `summary()` is test-only there (audit 2026-10-23 §F-063).
    #[test]
    fn the_import_report_is_rendered_at_the_edge() {
        let report = agnosis_core::package::ImportReport {
            imported: vec!["duck".to_string()],
            skipped: vec!["goose: a balloon of that name already exists, refused".to_string()],
            merged_assignments: 2,
            conflicts: Vec::new(),
        };
        let text = import_report_text(&report);
        assert!(text.contains("IMPORTED 1"), "{text}");
        assert!(text.contains("  duck\n"), "{text}");
        assert!(text.contains("SKIPPED 1"), "{text}");
        assert!(text.contains("  goose: a balloon"), "{text}");
        assert!(text.contains("ASSIGNMENTS 2"), "{text}");
        assert!(text.contains("CONFLICTS 0"), "{text}");
    }

    /// `agnosis config reset` without `--yes` refuses with exit code 2 —
    /// the code a script reads as "not confirmed" — and the refusal
    /// names the `.bak` that survives (audit 2026-10-23 §F-098). The
    /// refusal prints its lines on stderr as it runs.
    #[test]
    fn config_reset_refuses_without_yes() {
        assert_eq!(refuse_config_reset(), 2);
    }

    #[test]
    fn hidden_popup_parses_all_flags() {
        let parsed = parse_args(args(&[
            "agnosis",
            "popup",
            "--origin",
            "10,20",
            "--size",
            "1920,1080",
            "--scale",
            "1.5",
        ]))
        .expect("parse");
        match parsed.kind() {
            ParsedKind::Popup(popup) => {
                assert_eq!(popup.origin, (10, 20));
                assert_eq!(popup.size, (1920, 1080));
                assert!((popup.scale - 1.5).abs() < 1e-9);
            }
            other => panic!("expected popup, got {other:?}"),
        }
    }

    #[test]
    fn editor_parses_origin_and_size() {
        let parsed = parse_args(args(&[
            "agnosis",
            "editor",
            "--origin",
            "-1920,-40",
            "--size",
            "1920,1080",
        ]))
        .expect("parse");
        match parsed.kind() {
            ParsedKind::Editor(editor) => {
                assert_eq!(editor.origin, (-1920, -40), "a negative origin must parse");
                assert_eq!(editor.size, (1920, 1080));
            }
            other => panic!("expected editor, got {other:?}"),
        }
    }

    #[test]
    fn help_parses() {
        let parsed = parse_args(args(&["agnosis", "help"])).expect("parse");
        assert!(matches!(
            parsed.kind(),
            ParsedKind::Help {
                origin: None,
                size: None
            }
        ));
    }

    #[test]
    fn help_parses_the_spawned_geometry() {
        let parsed = parse_args(args(&[
            "agnosis",
            "help",
            "--origin",
            "-10,20",
            "--size",
            "1920,1080",
        ]))
        .expect("parse");
        assert!(matches!(
            parsed.kind(),
            ParsedKind::Help {
                origin: Some((-10, 20)),
                size: Some((1920, 1080)),
            }
        ));
    }

    #[test]
    fn install_and_stop_parse() {
        assert!(matches!(
            parse_args(args(&["agnosis", "install"]))
                .expect("parse")
                .kind(),
            ParsedKind::Install
        ));
        assert!(matches!(
            parse_args(args(&["agnosis", "stop"]))
                .expect("parse")
                .kind(),
            ParsedKind::Stop
        ));
    }

    #[test]
    fn invalid_subcommand_errors() {
        assert!(parse_args(args(&["agnosis", "frobnicate"])).is_err());
    }

    #[test]
    fn daemon_flag_parses() {
        let parsed = parse_args(args(&["agnosis", "--daemon"])).expect("parse");
        assert!(matches!(parsed.kind(), ParsedKind::Daemon));
    }

    #[test]
    fn add_parses_image_path() {
        let parsed = parse_args(args(&["agnosis", "add", "duck.png"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Add(path) => assert_eq!(path, &PathBuf::from("duck.png")),
            other => panic!("expected add, got {other:?}"),
        }
    }

    #[test]
    fn monitor_pick_respects_cursor_and_primary() {
        let cursor_cfg = agnosis_core::config::Config {
            monitor: agnosis_core::config::MonitorChoice::Cursor,
            ..Default::default()
        };
        let picked = pick_for_test(&cursor_cfg, &displays(), (2000, 500)).expect("pick");
        assert_eq!(picked.origin, (1920, 0));
        assert!((picked.scale - 1.25).abs() < 1e-9);
        let primary_cfg = agnosis_core::config::Config {
            monitor: agnosis_core::config::MonitorChoice::Primary,
            ..Default::default()
        };
        let picked = pick_for_test(&primary_cfg, &displays(), (2000, 500)).expect("pick");
        assert_eq!(picked.origin, (0, 0));
    }

    #[test]
    fn resolve_popup_target_returns_sane_size() {
        let cfg = agnosis_core::config::Config::default();
        let target = resolve_popup_target(&cfg).expect("resolve on Windows");
        println!(
            "RESOLVED TARGET: origin={:?} size={:?} scale={}",
            target.origin, target.size, target.scale
        );
        assert!(target.size.0 > 0 && target.size.1 > 0);
        assert!(target.scale.is_finite() && target.scale > 0.0);
    }

    #[test]
    fn pair_parsers_reject_garbage() {
        assert!(parse_pair_i32("10").is_err());
        assert!(parse_pair_i32("10,20,30").is_err());
        assert!(parse_pair_u32("0,1080").is_err());
        assert!(parse_pair_u32("1920,0").is_err());
        assert_eq!(parse_pair_i32("-1920,0").expect("negative"), (-1920, 0));
    }

    #[test]
    fn live_add_radii_use_scene_geometry() {
        let cfg = agnosis_core::config::Config::default();
        // Explicit-geometry helper: same balloon helpers as the live scene.
        let (rx, ry) = radii_for(&cfg, 1920, 1080);
        assert!(rx >= 2 && ry >= 1);
        assert_eq!(
            (rx, ry),
            (54, 36),
            "1080p fallback radii match the audit cell"
        );
        // Live path: resolves the real monitor (or the 1080p fallback with a
        // warn) so `add` bakes the radii the popup actually uses.
        let (lx, ly) = live_add_radii(&cfg);
        assert!(lx >= 2 && ly >= 1);
    }

    #[test]
    fn live_add_radii_match_the_resolved_monitor() {
        // `add` must stamp the cache with the radii of the monitor the popup
        // will use, not of a hardcoded 1080p (audit P0-3).
        let cfg = agnosis_core::config::Config::default();
        let target = resolve_popup_target(&cfg).expect("resolve");
        let expected = radii_for(&cfg, target.size.0, target.size.1);
        assert_eq!(live_add_radii(&cfg), expected);
    }

    /// A temp dir that is not the user's real balloon folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agnosis-cli-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    #[test]
    fn add_cache_lands_next_to_the_installed_art() {
        let dir = temp_dir("cache");
        let outside = temp_dir("outside");
        let source = outside.join("duck.png");
        std::fs::write(&source, b"not really a png").expect("write source");
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        let cache = cache_sprite_in(&dir, &source, &sprite, (54, 36)).expect("cache");
        // The art is installed in the balloon folder, the cache is its
        // sibling: that is the only place the watcher looks for it.
        let installed = dir.join("duck.png");
        assert!(installed.is_file(), "art must be copied into the folder");
        assert_eq!(cache, dir.join("duck.sprite.json"));
        assert_eq!(
            agnosis_core::image_ascii::stamped_radii(&cache),
            Some((54, 36)),
            "the cache must carry the live radii"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn add_keeps_art_that_already_lives_in_the_folder() {
        let dir = temp_dir("inside");
        let source = dir.join("duck.png");
        std::fs::write(&source, b"not really a png").expect("write source");
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        let cache = cache_sprite_in(&dir, &source, &sprite, (54, 36)).expect("cache");
        assert_eq!(cache, dir.join("duck.sprite.json"));
        assert!(source.is_file(), "the art must stay where it is");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_never_overwrites_an_existing_balloon() {
        let dir = temp_dir("clobber");
        let outside = temp_dir("clobber-src");
        let source = outside.join("duck.png");
        std::fs::write(&source, b"new art").expect("write source");
        let existing = dir.join("duck.png");
        std::fs::write(&existing, b"old art").expect("write existing");
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        let cache = cache_sprite_in(&dir, &source, &sprite, (54, 36)).expect("cache");
        assert_eq!(
            std::fs::read(&existing).expect("read existing"),
            b"old art",
            "the existing balloon must survive"
        );
        assert_eq!(cache, dir.join("duck-2.sprite.json"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn add_is_idempotent_for_the_same_art() {
        let dir = temp_dir("idem");
        let outside = temp_dir("idem-src");
        let source = outside.join("duck.png");
        std::fs::write(&source, b"same art").expect("write source");
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        let first = cache_sprite_in(&dir, &source, &sprite, (54, 36)).expect("first");
        let second = cache_sprite_in(&dir, &source, &sprite, (54, 36)).expect("second");
        assert_eq!(first, second, "the same art must not pile up copies");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // --- Stage 8: the swap feature (assign / list-drawn) --------------------

    #[test]
    fn assign_writes_the_config() {
        let dir = temp_dir("assign");
        let path = dir.join("config.toml");
        let message = assign_at(&path, "small_3", Some("cat"), false).expect("assign");
        assert!(message.contains("small_3"), "got {message}");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(
            text.contains("[assignments]"),
            "the table must exist: {text}"
        );
        assert!(
            text.contains("small_3 = \"cat\""),
            "the assignment must be written: {text}"
        );
        // The slot name is accepted in both spellings.
        let message = assign_at(&path, "SMALL3", Some("dog"), false).expect("assign");
        assert!(message.contains("small_3"), "got {message}");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(
            text.contains("small_3 = \"dog\""),
            "the assignment must be updated: {text}"
        );
        // A balloon slot lands in its own key.
        assign_at(&path, "balloon_3", Some("party"), false).expect("assign");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(
            text.contains("balloon_3 = \"party\""),
            "the balloon assignment must be written: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_grass_query_does_not_delete_the_tile() {
        let dir = temp_dir("grass-query");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[grass]\ntile = \"meadow\"\n").expect("write config");
        // The bare form is a QUERY: it answers which tile is in use and
        // writes nothing. It used to fall through to the write path with
        // an empty value, and the write dropped the key — so asking
        // deleted the setting (audit 2026-10-21 §G1).
        let code = grass_at(&path, None, false);
        assert_eq!(code, 0, "the query must succeed");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(
            text.contains("tile = \"meadow\""),
            "the query must not touch the config: {text}"
        );
        // The clear path really does remove the key.
        let code = grass_at(&path, None, true);
        assert_eq!(code, 0, "the clear must succeed");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(!text.contains("tile ="), "the key must be gone: {text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assign_clear_removes_the_entry() {
        let dir = temp_dir("assign-clear");
        let path = dir.join("config.toml");
        assign_at(&path, "big_1", Some("cat"), false).expect("assign");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(text.contains("big_1 = \"cat\""), "got {text}");
        let message = assign_at(&path, "big_1", None, true).expect("clear");
        assert!(message.contains("cleared"), "got {message}");
        let text = std::fs::read_to_string(&path).expect("read config");
        assert!(!text.contains("big_1 ="), "the entry must be gone: {text}");
        // Clearing a slot that was never set is not an error.
        assign_at(&path, "small_9", None, true).expect("clear");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn assign_rejects_an_unknown_slot() {
        let dir = temp_dir("assign-bad");
        let path = dir.join("config.toml");
        let err = assign_at(&path, "duck_3", Some("cat"), false).expect_err("unknown slot");
        assert!(err.contains("unknown slot"), "got {err}");
        assert!(err.contains("BIG1"), "the message must help: {err}");
        // An out-of-range number is refused as well.
        let err = assign_at(&path, "small_10", Some("cat"), false).expect_err("out of range");
        assert!(err.contains("out of range"), "got {err}");
        // A missing asset name is refused.
        let err = assign_at(&path, "small_3", None, false).expect_err("no name");
        assert!(err.contains("asset name"), "got {err}");
        // An illegal asset name is refused.
        let err = assign_at(&path, "small_3", Some("../escape"), false).expect_err("bad name");
        assert!(err.contains("illegal asset name"), "got {err}");
        assert!(
            !path.exists(),
            "a refused assignment must not write a config"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_drawn_lists_every_kind() {
        let sprites = temp_dir("list-sprites");
        let balloons = temp_dir("list-balloons");
        let decorations = temp_dir("list-decorations");
        std::fs::write(sprites.join("cat.json"), b"{}").expect("write sprite");
        std::fs::write(sprites.join("dog.json"), b"{}").expect("write sprite");
        std::fs::write(balloons.join("party.json"), b"{}").expect("write balloon");
        std::fs::write(decorations.join("crown.json"), b"{}").expect("write decoration");
        let out = list_drawn_in(&sprites, &balloons, &decorations);
        assert!(out.contains("SPRITES"), "got {out}");
        assert!(out.contains("cat"), "got {out}");
        assert!(out.contains("dog"), "got {out}");
        assert!(out.contains("BALLOONS"), "got {out}");
        assert!(out.contains("party"), "got {out}");
        assert!(out.contains("DECORATIONS"), "got {out}");
        assert!(out.contains("crown"), "got {out}");
        // An empty folder says so instead of printing nothing.
        let empty = temp_dir("list-empty");
        let out = list_drawn_in(&empty, &empty, &empty);
        assert!(out.contains("(none)"), "got {out}");
        let _ = std::fs::remove_dir_all(&sprites);
        let _ = std::fs::remove_dir_all(&balloons);
        let _ = std::fs::remove_dir_all(&decorations);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn assign_and_list_drawn_parse() {
        let parsed = parse_args(args(&["agnosis", "assign", "small_3", "cat"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Assign { slot, name, clear } => {
                assert_eq!(slot, "small_3");
                assert_eq!(name.as_deref(), Some("cat"));
                assert!(!clear);
            }
            other => panic!("expected assign, got {other:?}"),
        }
        let parsed = parse_args(args(&["agnosis", "assign", "--clear", "SMALL3"])).expect("parse");
        match parsed.kind() {
            ParsedKind::Assign { slot, name, clear } => {
                assert_eq!(slot, "SMALL3");
                assert!(name.is_none());
                assert!(clear);
            }
            other => panic!("expected assign, got {other:?}"),
        }
        assert!(matches!(
            parse_args(args(&["agnosis", "list-drawn"]))
                .expect("parse")
                .kind(),
            ParsedKind::ListDrawn
        ));
    }

    /// The `config` subcommand parses, and its half is carried through.
    #[test]
    fn the_config_subcommand_parses() {
        match parse_args(args(&["agnosis", "config", "show"]))
            .expect("parse")
            .kind()
        {
            ParsedKind::Config(ConfigCommands::Show) => {}
            other => panic!("expected config show, got {other:?}"),
        }
        match parse_args(args(&["agnosis", "config", "reset"]))
            .expect("parse")
            .kind()
        {
            ParsedKind::Config(ConfigCommands::Reset { .. }) => {}
            other => panic!("expected config reset, got {other:?}"),
        }
    }

    /// The track subcommand parses: list, show, use and clear.
    #[test]
    fn the_track_subcommand_parses() {
        match parse_args(args(&["agnosis", "track", "list"]))
            .expect("parse")
            .kind()
        {
            ParsedKind::Track(TrackCommands::List) => {}
            other => panic!("expected track list, got {other:?}"),
        }
        match parse_args(args(&["agnosis", "track", "use", "marching"]))
            .expect("parse")
            .kind()
        {
            ParsedKind::Track(TrackCommands::Use { name }) => assert_eq!(name, "marching"),
            other => panic!("expected track use, got {other:?}"),
        }
    }

    /// Choosing a track writes the `[audio] track` key and nothing else.
    ///
    /// The write is surgical, so the user's comments and the other tables
    /// survive — the test writes into a temp file, never the user's own.
    #[test]
    fn choosing_a_track_only_touches_the_track_key() {
        let dir = std::env::temp_dir().join(format!("agnosis-cli-track-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my ducks
loop_seconds = 12.0

[audio]
volume = 0.5
",
        )
        .expect("write");
        write_the_track_key(&path, "marching").expect("the key is written");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# my ducks"), "the comment survives");
        assert!(
            text.contains("loop_seconds = 12.0"),
            "the top level survives"
        );
        assert!(text.contains("volume = 0.5"), "the other keys survive");
        assert!(
            text.contains("track = \"marching\""),
            "the key is written, got {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `--daemon` and a subcommand cannot be combined: the flag used to
    /// swallow the subcommand silently (audit 2026-10-20 §3.18).
    #[test]
    fn the_daemon_flag_and_a_subcommand_do_not_mix() {
        // The conflict is caught before the daemon is started, so this
        // never spawns anything.
        let code = run_inner(
            args(&["agnosis", "--daemon", "install"])
                .into_iter()
                .map(std::ffi::OsString::from),
        )
        .expect("a code");
        assert_eq!(code, 2, "the conflict is a usage error");
    }
}

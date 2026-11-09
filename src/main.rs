// Release builds on Windows run windowed (popup/daemon) with no console
// window: the console subsystem otherwise allocates a second visible window
// next to the duck parade on direct launch (audit 2026-10-06 §3). Debug
// builds keep the console so `eprintln!` diagnostics stay usable in dev.
// Note: with this subsystem, release CLI subcommands print nothing.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod asset_loader;
mod audio;
mod backdrop_loader;
mod balloon_resolve;
mod cli;
mod config_watch;
mod daemon;
mod editor;
mod help;
mod host;
mod paths;
mod platform;
mod popup;
mod render;
mod shot_worker;
mod stale_stamp;
mod watcher;

fn main() {
    let code = cli::run();
    std::process::exit(code);
}

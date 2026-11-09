//! Build script: embed a Windows application manifest declaring
//! Per-Monitor-V2 DPI awareness.
//!
//! Without this manifest Windows DPI-virtualizes the process, so
//! `EnumDisplayMonitors` returns virtualized (smaller) monitor rects while
//! winit later creates the window in true physical pixels — the popup then
//! covers only part of the screen (audit 2026-10-06 §1). The manifest applies
//! before `main()` runs, covering every entry path (default popup, hidden
//! `popup` subcommand spawned by the daemon, selftest, tests).
//!
//! `embed-manifest` defaults to `DpiAwareness::PerMonitorV2Only`, which is
//! exactly what we want; nothing else is customized.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        embed_manifest::embed_manifest(embed_manifest::new_manifest("AGnosis"))
            .expect("unable to embed manifest file");
        // The application icon, as a Windows resource: Explorer, the task
        // bar, the alt-tab list and the installer's shortcuts all read it.
        // Build-time only — the runtime dependency set is unchanged.
        println!("cargo:rerun-if-changed=packaging/windows/agnosis.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("packaging/windows/agnosis.ico");
        res.compile()
            .expect("unable to embed the Windows resources");
    }
}

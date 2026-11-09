# Packaging — what ships, how it is built, how to check it

`agnosis` is one self-contained binary (the font is embedded with
`include_bytes!`, so there are no runtime asset files to ship). What differs
per platform is the *wrapper* around that binary: an Inno Setup installer
(or a portable zip) on Windows, an app bundle in a disk image on macOS, a
Debian package on Linux.

Everything in this directory is consumed by
[`.github/workflows/release.yml`](../.github/workflows/release.yml), which
turns a `v*` tag into the packages below; the Windows installer is built
locally (or by CI) with `windows/build-installer.ps1`.

| File | Used by | Purpose |
| --- | --- | --- |
| `macos/Info.plist` | the macOS job | Bundle metadata for `agnosis.app`. |
| `linux/agnosis.desktop` | the Linux job | The freedesktop entry that puts *AGnosis* in the application menu. |
| `linux/control.in` | the Linux job | The `DEBIAN/control` template of the `.deb`. |
| `windows/agnosis.iss` | the Windows installer | The Inno Setup 6 wizard: install-directory page, licence + README pages, autostart + daemon start, uninstaller. |
| `windows/build-installer.ps1` | the Windows installer | Release build + `ISCC`, with the version handed over as `/DAppVersion`. |
| `windows/agnosis.ico` | the exe + the installer | The application icon, generated from `REVIEW/agnosis.png`. |

---

## What ships on each OS

Three artifacts, one per platform, built by three runners:

| Artifact | Runner | What is inside |
| --- | --- | --- |
| `agnosis-<version>-windows-x64.zip` | `windows-latest` | `agnosis.exe` (x86-64, GUI subsystem) + `LICENSE.txt` |
| `agnosis-<version>-macos-universal.dmg` | `macos-latest` | `agnosis.app` — a universal (x86_64 + arm64) binary in an app bundle |
| `agnosis_<version>_amd64.deb` | `ubuntu-latest` | `ducks` on `PATH`, a freedesktop entry, the README and the licence |

`<version>` is the tag without its `v` (`v0.1.0` → `0.1.0`); a
`workflow_dispatch` run takes the version from `Cargo.toml` instead.

### Windows — the installer `agnosis-setup.exe` (recommended)

`windows/agnosis.iss` is an **Inno Setup 6** script: a per-user wizard (no
elevation) that asks **where to install** (`%LOCALAPPDATA%\Programs\AGnosis`
by default), shows the licence and the README in the wizard, copies
`agnosis.exe` + `README.md` + `LICENSE.txt`, and offers Start-Menu and
desktop shortcuts. Its finish page offers the three setup steps the app
itself owns — `agnosis install` (login autostart), `agnosis start` (daemon
now, hotkey live), `agnosis help` (the README window) — and the uninstaller
stops the daemon and reverses the autostart first.

Build it with:

```powershell
powershell -ExecutionPolicy Bypass -File packaging/windows/build-installer.ps1
```

The script runs `cargo build --release`, reads the version from `Cargo.toml`
and hands it to `ISCC` (`/DAppVersion=`), so installer and binary never
disagree. Inno Setup 6 comes from `winget install JRSoftware.InnoSetup`. The
built `agnosis-setup.exe` lands in `windows/dist/` (gitignored — it is a
release artifact).

### Windows — `agnosis-<version>-windows-x64.zip`

```
agnosis.exe          the release binary (x86_64, GUI subsystem)
LICENSE.txt          the licence text (Open Source License with Ethical Restrictions)
```

The zip is portable: unzip it anywhere and run `agnosis.exe`. Nothing else is
needed — the font is embedded in the binary, so there are no runtime asset
files to ship.

**The binary is a GUI-subsystem binary.** It does not open a console window,
and the CLI verbs print *nothing* by design — a double-clicked `agnosis.exe`
must not flash a black console next to the duck parade. The only command
that writes anything is the headless selftest:

```powershell
.\agnosis.exe selftest --frames 2 --out out
```

It writes `out/frame_0000.png`, `out/frame_0001.png`, … and prints a status
line that is only visible when the process has a console (a terminal, a
redirected file). Assert on the **PNG files**, never on stdout.

### macOS — `agnosis-<version>-macos-universal.dmg`

A compressed disk image holding one item:

```
agnosis.app/
  Contents/
    Info.plist
    MacOS/agnosis        universal binary (x86_64 + arm64)
    Resources/
```

`Resources/` is empty (the font is embedded), and the bundle version in
`Info.plist` is rewritten from the crate version at build time, so the
shipped app never carries a stale `CFBundleVersion`.

Drag `agnosis.app` into `/Applications`.

**The app is unsigned and un-notarised.** Notarisation needs an Apple
developer account, which is out of scope for this project, so Gatekeeper
blocks the first launch. Either:

* right-click `agnosis.app` → **Open** → **Open** in the confirmation dialog, or
* launch it once from **System Settings → Privacy & Security**, where
  macOS lists the blocked app behind an **Open Anyway** button.

**The global hotkey needs the Input Monitoring grant.** macOS only lets an
app read the keyboard after the user allows it explicitly:

> **System Settings → Privacy & Security → Input Monitoring** → add *AGnosis* →
> switch it on.

The grant is keyed on the bundle id `com.agnosis.daemon` (the same string as
`platform::APP_BUNDLE_ID`), so it survives an upgrade of the app — but a
bundle-id change silently drops it. The **menu-bar item** is the
permission-free path: it starts the parade without asking for any privacy
grant.

**`agnosis install` registers the menu bar on macOS, not the daemon.** The
LaunchAgent plist it writes (`~/Library/LaunchAgents/com.agnosis.daemon.plist`)
launches `agnosis menubar`, so the status-bar icon appears at login and the
parade and the *Draw Your Own* editor stay reachable even when the *Input
Monitoring* grant is missing. The daemon — and with it the global hotkey —
is therefore **not** started at login on macOS: run `agnosis start` for the
chord (Windows and Linux register `agnosis --daemon` instead).

### Linux — `agnosis_<version>_amd64.deb`

```
agnosis_<version>_amd64.deb
  DEBIAN/control
  usr/bin/agnosis
  usr/share/applications/agnosis.desktop
  usr/share/doc/agnosis/README.md
  usr/share/doc/agnosis/copyright
```

`agnosis.desktop` is the freedesktop entry that puts *ducks* in the
application menu; it launches `agnosis` with `Terminal=false`, so no console
window flashes next to the parade.

Install it with:

```bash
sudo dpkg -i agnosis_0.1.0_amd64.deb
```

`agnosis` lands on `PATH` and the desktop entry shows up in the application
menu (as soon as the session reloads it; `update-desktop-database
/usr/share/applications` — from the `desktop-file-utils` package — forces
that).

**The binary needs the X11 runtime libraries** — the build uses winit's
`x11` backend, so `libx11-6`, `libxext6`, `libxfixes3`, `libxcursor1`,
`libxi6`, `libxrandr2`, `libxcb1` and `libxkbcommon-x11-0` must be
installed. The `Depends:` line of `DEBIAN/control` names them, and the CI
job checks that line twice: against the **link-time** closure `ldd`
reports, and against the hand-written list of the X11 libraries winit
`dlopen`s at runtime — `ldd` never reports those, so the second check is
the half that catches a package that installs but will not start (see
below).

**On a Wayland-only session the X11 backend runs through XWayland.**
Install `xwayland` (Debian/Ubuntu: `sudo apt install xwayland`) and make
sure `DISPLAY` is set; a session without an X server has no display for the
popup to open on.

The `.deb` is Debian-family only. Other distributions need the AppImage or
a plain binary.

---

## How the artifacts are built

`.github/workflows/release.yml` runs on a `v*` tag (or on demand through
`workflow_dispatch`) and has four jobs:

1. **windows** (`windows-latest`) — `cargo build --release --locked`, then the
   selftest smoke test, then the zip.
2. **macos** (`macos-latest`) — one build per architecture
   (`aarch64-apple-darwin` and `x86_64-apple-darwin`), merged with
   `lipo -create`, assembled into `agnosis.app`, and imaged with `hdiutil`.
   The `Info.plist` version is rewritten from the crate version so the
   bundle never carries a stale `CFBundleVersion`.
3. **linux** (`ubuntu-latest`) — `cargo build --release --locked`, then the
   package tree, then `dpkg-deb --build --root-owner-group`.
4. **release** — downloads the three artifacts and attaches them to the
   GitHub release for the tag (`gh release create --generate-notes`, or
   `gh release upload` when the release already exists).

The version comes from the tag (`v0.1.0` → `0.1.0`); a `workflow_dispatch`
run takes it from `Cargo.toml` and attaches the packages to the run
**without** publishing a GitHub release.

The Linux runner needs the X11 headers and `dpkg-dev` before it can build
the package (`sudo apt-get install -y libx11-dev libxext-dev
libxfixes-dev dpkg-dev`); the macOS runner needs both Darwin targets
installed on the toolchain (`aarch64-apple-darwin`, `x86_64-apple-darwin`),
because macOS cannot be cross-compiled to from Linux or Windows.

Every package job runs the same **smoke test** before it packs: the
headless selftest has to write at least two PNG frames. The Windows job
cannot look at stdout — the release binary is a GUI-subsystem binary and
prints nothing — so it asserts on the PNG files.

Two details worth knowing:

* **The macOS image is built from a staging directory.** `hdiutil create
  -srcfolder` copies the *contents* of the folder into the image root, so
  `-srcfolder agnosis.app` would put the bundle's inner `Contents` directory
  at the volume root instead of `agnosis.app`. The workflow therefore stages
  `agnosis.app` inside `dmgroot/` and passes that directory to `-srcfolder`,
  and then mounts the image again to assert that `agnosis.app` is really at
  the root.
* **The `Depends:` line is asserted twice.** The guard against a package
  that installs but will not start has two halves, because one check cannot
  see the whole dependency set:
  * the **link-time closure**: the Linux job runs `ldd target/release/agnosis`,
    maps every shared library it reports to a Debian package, and fails the
    build when the `DEBIAN/control` template does not name that package;
  * the **X11 runtime list**: winit is built with the `x11-dl` feature, so
    `libX11`, `libXcursor`, `libXrandr`, `libXi`, `libXfixes` and
    `libxkbcommon-x11` are `dlopen`ed at runtime — `ldd` never reports
    them. The job therefore also fails the build when one of the
    hand-written X11 packages is missing from `Depends:`, and the message
    names the missing package.

`control.in` is a template: `@VERSION@` and `@INSTALLED_SIZE@` are
substituted at build time by the workflow.

---

## Verifying an artifact locally

### Windows zip

```powershell
Expand-Archive agnosis-0.1.0-windows-x64.zip -DestinationPath agnosis
cd agnosis
Get-ChildItem                        # agnosis.exe, LICENSE.txt
.\agnosis.exe selftest --frames 2 --out out
Get-ChildItem out\*.png      # frame_0000.png, frame_0001.png
```

The PNGs appearing is the check — the binary prints nothing on the console.

### macOS disk image

```bash
hdiutil attach agnosis-0.1.0-macos-universal.dmg
ls /Volumes/ducks                       # agnosis.app
lipo -info /Volumes/ducks/agnosis.app/Contents/MacOS/ducks
# Architectures in the fat file: ... are: x86_64 arm64
/Volumes/ducks/agnosis.app/Contents/MacOS/ducks selftest --frames 2 --out /tmp/ducks-selftest
hdiutil detach /Volumes/agnosis
```

Then open the app: right-click → **Open** (unsigned app), and grant
**Input Monitoring** under *System Settings → Privacy & Security → Input
Monitoring* if you want the global hotkey. The menu-bar item (*ducks* 🦆 in
the status bar) needs no grant and is the fallback when the grant is
refused.

### Linux package

```bash
dpkg-deb --contents agnosis_0.1.0_amd64.deb   # the package tree
sudo dpkg -i agnosis_0.1.0_amd64.deb
which agnosis
ducks selftest --frames 2 --out /tmp/ducks-selftest
ldd "$(which ducks)" | sed -n 's/.*=> \(.*\)/\1/p'   # every library is installed
dpkg -s ducks | grep '^Depends:'                     # matches the list above
```

The `.deb` installs `ducks` on `PATH`; the desktop entry appears in the
application menu.

`ldd` only lists the **link-time** closure — the X11 libraries winit
`dlopen`s at runtime (`libX11`, `libXcursor`, `libXrandr`, `libXi`,
`libXfixes`, `libxkbcommon-x11`) never show up there, which is why the CI
asserts the hand-written list as well. Check that half by hand too:

```bash
dpkg -s ducks | grep '^Depends:' | tr ',' '\n' | grep -E 'libx11-6|libxcursor1|libxrandr2|libxi6|libxfixes3|libxkbcommon-x11-0'
```

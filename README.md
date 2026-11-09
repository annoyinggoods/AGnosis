# AGnosis

![The AGnosis logo](REVIEW/ag-logo.png)

## ABOUT

`agnosis` hides your browser behind a fullscreen ASCII duck parade. It ships
two ways: a **Windows installer** that sets everything up for you (install
folder, Start-Menu shortcut, login autostart, daemon running), and a **single
portable exe** you can run from anywhere with no install at all.

The **daemon** is the resident half: it owns the global hotkey, and pressing
that hotkey minimizes every open browser window and opens the parade. The
**parade itself** — and everything you make for it — needs no daemon: the
editor, the balloon folders and the exporters all work from a bare exe.

When the hotkey fires, every open browser window is **minimized**, and a
borderless fullscreen window takes over the display: a band of grass, 11
ducks marching right-to-left led by two large ones, and balloons trailing
behind on simulated wobbling ropes.

It is the old Windows XP "hide your browser behind a fake hacker terminal"
joke, with one difference that matters: **nothing is ever closed**. The
browsers are only minimized, so no tab, no form and no unsaved work is
ever lost — and by default they **stay** minimized when the parade ends,
because a browser that pops back up is exactly the "parent walked in on
you" moment the joke is built on. (You can turn the automatic restore on
if you would rather have your window state back — see HOW TO USE point 1.)

The parade is yours to make: drop a picture in to turn it into a balloon,
or draw your own duck in the built-in editor.

## FEATURES SUMMARY

i. The app **minimizes** all open browsers when the parade starts, but they
    **remain running** — see HOW TO USE point 1 to change what happens when
    the parade ends. **NO WORK IS LOST AS A RESULT: this is minimize only,
    nothing is ever closed.**
ii. A fullscreen ASCII duck parade: a band of grass, 11 ducks led by two
    big ones, balloons on ropes, and an on-screen exit hint.
iii. **Any picture becomes a balloon** — drop it in the balloons folder, or
    type its name on the parade's input line.
iv. **Draw your own** duck or balloon in the built-in editor — **`F2` opens
    it straight from the parade** — and save it.
v. **Point any parade slot at your own art** — the two big ducks, the nine
    small ducks and the five balloons.
vi. A **global hotkey** on Windows, Linux and macOS, plus a
    **menu-bar trigger** on macOS that needs no privacy grant.
vii. **One config file** (`config.toml`) for everything, with a repair pass
    that keeps it valid and writes the fixes back.
viii. A **headless PNG renderer** (`agnosis selftest`) for scripting, for
    previews and for the release gate.
ix. A **live config reload**: edit `config.toml` while the parade runs and
    the change lands within a second — no restart.
x. **Content you can change**: a sky gradient, a custom message, a drawn
    grass band, per-duck personalities, and the weather (rain, snow, wind,
    lightning).
xi. **Take it with you**: export and import a package (the drawn art, the
    assignments and the look), save scene presets, and copy a frame to the
    clipboard.
xii. **The editor never loses your work**: undo and redo a whole stroke
     (`Ctrl+Z`), and a draft is written a second after you stop drawing,
     so a crash costs at most the last second.
xiii. **A settings panel instead of a TOML file**: click a value and the
      parade changes while it runs — and your comments and unknown keys
      survive the write.
xiv. **Put a hat on the ducks**: a decoration is drawn over their bodies,
     one per duck size.
xv. **The ducks notice you**: click one and it hops, grimaces and clucks;
     shift-click and it stops in place and the ducks behind it stop too.
xvi. **A voice**: every sound is synthesized (no audio files ship), a
      MIDI note track can be placed on a strip in the editor and played
      in step with the marching ducks, and a machine with no sound card
      simply runs in silence.
xvii. **A backdrop behind the ducks**: point the config at a picture and
      it becomes a scrolling layer of ASCII art, at its own speed, behind
      the parade.
xviii. **Send it to somebody**: `agnosis export parade.html` writes a
      single self-contained page (no network, no assets, no fonts) that
      replays the loop and can be paused and scrubbed, and
      `agnosis export parade.gif` writes an animated GIF of the same loop.
xix. **The same parade in a browser tab**: the simulation is a library,
      so the web build is a *second front end* over it — the same ducks,
      the same physics, the same art, drawn with WebGPU, WebGL2 or a
      plain 2D canvas. Read-only: the art travels out as a package and
      back in through a file input.

## HOW TO USE

1) **The app minimizes all the opened browsers.** They are minimized, never
    closed — the processes keep running and no work is lost. **By default
    they stay minimized after the parade ends**, which is the joke: the
    browser is still hidden when the "parent" walks in, and you un-minimize
    it yourself when you want it back. If you would rather have the
    windows restored automatically, set this in `config.toml`:

    ```toml
    [browsers]
    restore_on_close = true
    ```

    The file lives next to the balloons folder
    (`%APPDATA%\AGnosis\AGnosis\config\config.toml` on Windows,
    `~/.config/AGnosis/config.toml` on Linux,
    `~/Library/Application Support/com.agnosis.agnosis/config.toml` on macOS).
    Run `agnosis config show` to see the values the app actually uses.

2) **Press the global hotkey to start the parade, and press it again to
    stop it.** `ESC` closes the parade too, and the chord is per-platform
    (`A+G+Shift+Space` on Windows and Linux, `Ctrl+Alt+Shift+Space` on
    macOS) — see *Hotkey Chords per Platform* below. The parade follows the
    pointer (`monitor = "cursor"`) or the primary display.

3) **Make a balloon out of a picture.** Drop the image into the balloons
    folder, or type its name on the parade's input line and press `Enter`.
    `Ctrl+O` opens the balloons folder for you. See *Adding Balloon
    Pictures* below.

4) **Draw your own duck.** **`F2` in the parade** opens the editor window (the
    `Ctrl+Shift+D` chord, or `agnosis editor`, also work): a canvas, 19
    drawing tools, 8 colours, and a save button. See *Draw Your Own* below.

5) **Put your art in the parade.** The editor's **ASSIGN** button points
    any of the sixteen parade slots at a drawn asset; `agnosis assign` and
    `agnosis list-drawn` do the same from the command line.

6) **Tune it.** Everything lives in `config.toml` — the march speed, the
    balloon radius, the ropes, the colours, the exit hint, the hotkey
    chords, the browser list. See *Configuration* below.

7) **Make it yours.** The sky, a custom message, a drawn grass band, the
    weather and per-duck personalities all live in `config.toml` — and
    they land **live**: edit the file while the parade runs and the
    change is on screen within a second. See *Make It Yours* below.

8) **Start it at login.** `agnosis install` registers a login item (on macOS
    it registers the menu bar). See *Install* below.

---

## Command Reference

`agnosis` is one exe with subcommand dispatch:

```bash
agnosis [OPTIONS] [COMMAND]
```

### Commands

| Command | Description |
| --- | --- |
| `agnosis` | Default invocation. Renders the fullscreen popup immediately on the configured monitor. |
| `agnosis --daemon` | Runs the resident background daemon listening for the global hotkey. |
| `agnosis install` | Registers a login item so *AGnosis* comes up on user login. **Windows and Linux** start the daemon (`agnosis --daemon`); **macOS starts the menu bar** (`agnosis menubar`) — the menu bar needs no privacy grant, so the parade and the editor stay reachable. On macOS the daemon (and with it the global hotkey) is therefore **not** started at login: run `agnosis start` for the chord. |
| `agnosis uninstall` | Removes the login autostart registration and stops any running daemon (retains balloon files). |
| `agnosis start` | Spawns a detached background daemon. Reports `daemon already running` when one is live. |
| `agnosis stop` | Requests a running daemon to shut down cleanly. Reports `no daemon running` or `no daemon running (stale pid file removed)`. Exits 0 when a daemon was signalled (or a stale record was cleaned up) and 1 when nothing was running. |
| `agnosis add <image>` | Converts an image (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`) from anywhere on disk into a balloon: copies it into `<config>/balloons/` and writes a radii-stamped `.sprite.json` cache beside it. |
| `agnosis add-from-text <file.ans>` | Converts a hand-crafted ANSI/ASCII text file into a balloon sprite. |
| `agnosis selftest [--frames N] [--out DIR] [--config FILE]` | Renders headless scene frames to PNG without opening a window or requiring a GPU (defaults: `--frames 30 --out ./selftest_out`). `--config` renders a specific configuration instead of the built-in one. |
| `agnosis editor --origin X,Y --size W,H` | Opens the "Draw Your Own" editor window, where you hand-draw a sprite or a balloon and save it. This is the spawn target the daemon uses for the `Ctrl+Shift+D` chord; `--size` is the **monitor** size in physical pixels (the editor sizes its own window). |
| `agnosis assign <slot> <name>` | Points a parade slot at a drawn asset. `slot` is `BIG1`–`BIG2`, `SMALL1`–`SMALL9` or `BALLOON1`–`BALLOON5`; `agnosis assign --clear <slot>` puts the built-in art back. It rewrites **only the `[assignments]` table**, in place, so your comments and every key `agnosis` does not know survive; the write is atomic and keeps the previous file as `config.toml.bak`. |
| `agnosis list-drawn` | Prints the names of the assets you have drawn, under `SPRITES`, `BALLOONS` and `DECORATIONS`. |
| `agnosis config show` | Prints the configuration the app actually uses — the file after the out-of-range values were repaired — **and** the repairs it applied. It writes nothing. |
| `agnosis config validate` | Parses **your** `config.toml` the way the parade's live reload does and prints every repair it would apply (or the parse error). Exit 1 when the file does not parse — the same edit the parade answered with `CONFIG ERROR - OLD VALUES KEPT` on its input line. |
| `agnosis config reset` | Writes the default configuration back, keeping the previous file as `config.toml.bak`. Destructive, so it is **refused without `--yes`**: the refusal says what dies (colours, hotkeys, assignments, personalities) and names the `.bak` that survives. This is the way back from a config the app refuses to load. |
| `agnosis config backup` | Copies the current `config.toml` to a **timestamped** file (`config-2026-10-20-1912.toml`) beside it and prints the path. Deliberately not the `.bak`, which the writer overwrites. |
| `agnosis config export <file>` | Writes the configuration's **manifest** (the schema the package, the preset and the share file all use) to a file. |
| `agnosis export <file>` | Writes a shareable file. The **extension** picks the format: `.html` writes the **self-contained page** (the loop, in one file, with no network and no assets), `.gif` writes the **animated GIF**, and every other extension writes the **package** (the drawn art, the assignment table and the look). `--fps`, `--frames` and `--scale` tune the two new formats; `--name` names the creation in the manifest and the page title instead of the default `agnosis package`. |
| `agnosis import <file>` | Reads a package back on another machine: the drawn art, the assignment table and the look. An asset whose name already exists is **refused** rather than overwritten, and your own assignments always win. |
| `agnosis preset save <name>` / `list` / `use <name>` | Saves the current look under a name, lists the saved presets, and applies one. A preset replaces the **look** (sky, weather, message, grass, assignments, personalities) and never touches the hotkey or the browser list. |
| `agnosis grass <name>` | Uses a drawn grass tile for the ground band; `agnosis grass --clear` puts the built-in band back. With **no** name it is a pure query: it prints the tile in use and writes **nothing** to your config. |
| `agnosis shot <file.png>` | Writes a frame of the parade to a PNG file. The popup's own **F12** key copies the *live* frame to the clipboard. |
| `agnosis help` | Opens the **in-app HELP window**: the full text of this README, scrollable (the parade's own **F1** key opens the same window, and a second press while a reader is open is reported, not stacked). The window reads `README.md` **at open time** — beside the binary, or beside the repo checkout for development builds — so what it shows is always the documentation that ships; a missing, empty **or deleted** file falls back to the copy embedded at build time, and the embedded copy is written back beside the binary so the file never stays deleted. ESC closes it, the wheel and PgUp/PgDn scroll, Home/End jump. |
| `agnosis track list` / `show <name>` / `use <name>` / `clear` | The **MIDI note tracks**: `list` prints the tracks in `<config>/tracks/`, `show` prints the notes of one, `use` makes the parade play it (it writes the `[audio] track` key) and `clear` stops the track. The tracks are made in the editor's **TRK** mode. |
| `agnosis menubar` | Runs the macOS menu-bar trigger (*Show Parade* / *Draw Your Own* / *Stop Parade* / *Quit*). It needs no privacy grant, so it works even when the global hotkey cannot start. On every other platform it answers "the menu bar is macOS only". |

Pressing `ESC` while the popup is focused exits the popup. Pressing the global hotkey while a popup is already active toggles it shut. The editor chord (`Ctrl+Shift+D` by default) toggles the editor window instead and never minimizes your browsers.

---

## Adding Balloon Pictures

There are three ways to turn a picture into a balloon. All of them convert the image (`.png`, `.jpg`, `.jpeg`, `.gif`, `.bmp`) into ASCII art and mount it on a duck's rope.

### 1. Type it live on the input line

While the popup is open, the line below the grass reads
`ADD BALLOON: NAME + ENTER - CTRL O FOLDER - F12 SHOT - F2 EDITOR - F1 HELP`.

1. Put the picture in the **balloons folder** (see below) — or press **`Ctrl+O`** while the popup is open, and `agnosis` opens that folder for you. The shortcut carries a modifier on purpose: a bare letter would make every picture name that starts with that letter untypeable.
2. Type the picture's name (e.g. `redball`) — or a full path to any image on disk. A bare name is matched case-insensitively against every picture in the balloons folder and its subfolders, so `REDBALL` finds `redball.png`.
3. Press **`Enter`**. The line reports `ADDED <NAME>` and the balloon appears on the next carrier duck. `CANNOT LOAD ...` means the name did not resolve or the format is unsupported.

The typed name resolves exactly like a file dropped into the folder: a `<name>.sprite.json` cache next to the file is reused only when it was stamped with the radii of the current screen geometry, and an unstamped or mismatched cache is re-converted.

While you type, the line shows your text (`ADD BALLOON: redball_`); `Backspace` deletes. The buffer holds 64 characters, and on a narrow grid the line scrolls so the newest characters stay visible. `ESC` still exits the popup first.

### 2. Drop pictures into the balloons folder

The balloons folder lives next to the config (the popup's `Ctrl+O` opens it):

- **Windows:** `%APPDATA%\AGnosis\AGnosis\config\balloons`
  (e.g. `C:\Users\<User>\AppData\Roaming\AGnosis\AGnosis\config\balloons`)
- **Linux:** `~/.config/AGnosis/balloons`
- **macOS:** `~/Library/Application Support/com.agnosis.agnosis/balloons`

A directory watcher picks up any image or `.ans` file dropped there at runtime — no restart needed.

### 3. The `agnosis add` command

`agnosis add <image>` turns a picture from anywhere on disk into a balloon:

1. The real target monitor is resolved (the same probe the popup uses) and the sprite is converted at that monitor's radii. Art baked for another screen size would show up at the wrong size.
2. The picture is copied into the balloons folder (art that already lives there stays where it is), so the watcher picks it up. An existing balloon is never overwritten: when the name is taken by different art, the copy becomes `<stem>-N.ext` (`N` starts at 2).
3. The converted sprite is written as `<stem>.sprite.json` **next to the picture**, stamped with the radii it was built for. The watcher only reads a cache that sits next to the image, and re-converts when the stamp does not match the live geometry.

`agnosis add-from-text <file.ans>` does the same for a hand-made `.ans` file.

> Balloons fill a ring of 5 slots on ducks 1, 3, 5, 7 and 9; a 6th picture replaces the oldest one.

---

## Draw Your Own

The **Draw Your Own** editor is a separate window (its own process, its own
window class) in which you hand-draw an ASCII sprite or a balloon, save it,
and put it into the parade.

### Opening it

- **`Ctrl+Shift+D`** — the editor chord (`[hotkey].editor`). It **toggles** the
  editor and never minimizes your browsers; that is the parade hotkey's job.
- **The macOS menu bar** — *Draw Your Own* in the `agnosis` menu-bar item
  (`agnosis menubar`). It needs no privacy grant, so it works even when the
  global hotkey cannot start.
- **`agnosis editor --origin X,Y --size W,H`** — the spawn target the daemon
  uses. `--size` is the **monitor** size in physical pixels; the editor
  sizes and centres its own window on it. The command is hidden from
  `agnosis --help` (it is an internal spawn target), but it still works.

### The window is borderless

The editor window has **no title bar, no close button and no system menu** —
it is borderless on purpose, so that the mouse coordinates the window reports
line up with the canvas pixels. It is not fullscreen either: it is sized to
80 % of the monitor (at least 1122 × 600, at most 1920 × 1200) and centred on
it. The floor is the width the top bar's controls need, so the name field
can never be squeezed out of sight while it is being typed into; the OS
enforces it, so the window cannot be dragged below it.

That means **there is no close button to hunt for**: press **`Escape`** to
close the editor, or press the editor chord (`Ctrl+Shift+D`) again to toggle it
shut. The window is still resizable: it keeps a thin, invisible border you
can drag to resize, and the canvas, the tool panel and the status line
follow the new size.

### Drawing

The top bar carries three dropdowns, the asset-name field and the
`CFG` / `ASSIGN` / `DRAW` mode buttons (see *Settings mode* and *Assign
mode* below):

| Control | Values | Notes |
| --- | --- | --- |
| 1 | `BAL` / `SPR` / `GRS` / `DEC` | a balloon, a walking sprite, a grass tile or a duck decoration |
| 2 | `BIG` / `SML` | the duck size — greyed out while a balloon is picked |
| 3 | `1` … `4` | one static frame, or 2–4 animation poses — greyed out while a balloon is picked |

The canvas is the element's own cell grid: **150 × 150 cells** for the big
duck, 75 × 75 for the small one, and for a balloon the **disc** the parade
draws — 109 × 73 cells at the default balloon radius. Only the frame
rectangle at the top-left is drawable (150 × 70 for the big duck, 75 × 35 for
the small one); for a balloon only the disc is. Everything else is plain
background and cannot be painted.

A balloon's canvas is not square, and that is the point: a character cell
is taller than it is wide, so a disc that is 3:2 **in cells** is a
**circle on screen** — exactly like the balloon the parade shows. The art
you draw is mapped onto the runtime disc cell for cell, so a drawn balloon
is never stretched or squashed. The editor window opens wide enough for
the whole disc; on a small screen the canvas is clipped at the window edge
(as it always was for the sprite canvas).

The strip to the right of the canvas holds the 19 tool glyphs and the 8
colours, and marks the selected tool and colour. Clicking a cell paints it
with the active glyph in the active colour (the default is `x` in white).
The mouse wheel scrolls the canvas — and, when the pointer sits over the
tool/colour panel, it scrolls that panel instead, so a long tool list stays
reachable in a short window. `Enter` stores the current frame and
moves on to the next one — the previous pose stays visible as a faint ghost
— and the last frame is written to disk. `Escape` closes the editor.

The **`F1`–`F4`** keys reach every panel without the mouse (the panels used
to be mouse-only): `F1` the settings, `F2` the assignment picker, `F3` the
note strip, `F4` back to the canvas. In the settings panel the rows answer
the keyboard too: the arrow keys move the cursor, `Left`/`Right` step the
value of the row the cursor is on, and `Enter`/`Space` activate it (a
toggle flips, a dropdown opens). `Escape` walks a ladder — it closes an
open dropdown first, commits a half-typed field, declines a pending draft
offer (the draft file is kept), then leaves the panel — and only closes
the editor from a plain canvas.

Each asset is one JSON document, and the runtime frame is **always** the
built-in size (150 × 70 cells for the big duck, 75 × 35 for the small one;
the disc's own 109 × 73 cells for a balloon), so a drawn duck stands in
exactly the same cells of the train as the built-in art and the parade
spacing never changes.

### Assign mode

The top bar's **`ASSIGN`** button switches the editor from drawing to
assignment: the canvas area becomes the assignment panel — the sixteen
parade slots on the left (`BIG 1`, `BIG 2`, `SMALL 1` … `SMALL 9`,
`BALLOON 1` … `BALLOON 5`) and, on the right, the drawn assets of the
selected slot's kind (sprites for the ducks, balloons for the balloon
slots). Click a slot to select it and click an asset to point the slot at
it: the editor writes the `[assignments]` table into your `config.toml` and
the status line reports `ASSIGNED <SLOT> = <NAME>`. The **`CLEAR`** button
below the slot list takes the assignment out again. The **`DRAW`** button
goes back to the canvas, and the drawing you left is still there.

Below the parade slots the same panel lists the three **look targets**:
`GRASS` (the tile the parade tiles its ground with), `DECO BIG` and
`DECO SMALL` (the decorations the big and the small ducks wear). They work
the same way — pick the target, click an asset of its kind (grass tiles or
decorations), and the editor writes the one key (`[grass] tile`,
`[decorations] big`, `[decorations] small`) surgically into your config;
`CLEAR` puts the built-in look back.

### Settings mode

The top bar's **`CFG`** button switches the editor from drawing to the
settings panel: the canvas area becomes a list of rows, one per
documented setting — a label, a value and a unit. A numeric setting is a
**slider** (click or drag the track), a boolean is a **toggle**, an enum is
a **dropdown** and a string is a **text field** (click it, type, press
`Enter` to commit). The arrow keys move the cursor from row to row, and
the mouse wheel scrolls the list.

Every change is written into **your** `config.toml` **surgically**: only
the one key is replaced, so your comments, your unknown keys and every
other setting survive byte-for-byte, and a `config.toml.bak` is kept
beside the file. A value that cannot be written leaves the panel showing
the old value and the toast explaining why — the editor never shows a
value it failed to store. The parade picks the change up through the live
reload, so the balloon radius moves while the parade is running.

The legacy `cell_size` key is deliberately **not** offered: it is ignored
by the app since the row-budget grid landed.

The panel also carries the **PERSONALITY** actions: `RANDOMISE` gives each
of the eleven ducks its own personality (a small delta around the built-in
behaviour — bob, gait speed, phase, hop — written as `[[personality]]`
tables into your config, seeded from the clock so every press differs),
and `RESET` removes every `[[personality]]` table so all ducks return to
the built-in behaviour. There is no per-duck picker; hand-tune the tables
in the config file for that.

### Undo, redo and the draft

`Ctrl+Z` undoes the last **stroke** and `Ctrl+Shift+Z` redoes it (`Cmd+Z`
works too on macOS). A stroke is one press-and-drag: a drag that paints
200 cells is **one** undo step, not two hundred. The stack holds 200
strokes and drops the oldest beyond that.

The editor also keeps a **draft**: a second of quiet after a stroke
writes the whole drawing to `<config>/drawn/drafts/<kind>-<name>.draft.json`,
so a crash (or the release build's `panic = "abort"`) costs at most the
last second of work. The draft lives in its own folder, so the parade and
`agnosis list-drawn` can never pick a half-finished drawing up as if it
were a real asset. When the editor opens and finds a draft it **offers**
it — the status line says `DRAFT FOUND - PRESS ENTER TO LOAD` and `Enter`
restores the drawing exactly. Saving the asset deletes the draft.

### Decorations

A **decoration** is a second frame-sized asset that is drawn **over** the
duck's body: a hat, a scarf, a bowtie. The kind dropdown's fourth option
is `DEC`. A decoration is authored on the duck's **own frame** (150 × 70
for the big duck, 75 × 35 for the small one) with the built-in art shown
faintly underneath, so you can see where the duck's head is. The blank
cells stay transparent, so the body shows through everywhere the
decoration does not cover.

A decoration's frames step in step with the body's, so a walking duck
keeps its hat in place. The `[decorations]` table in `config.toml` names
one decoration per duck size:

```toml
[decorations]
big = "crown"
small = "bowtie"
```

A decoration that does not exist leaves the ducks bare — the built-in art
is untouched.

### Where the drawn assets live

- **Windows:** `%APPDATA%\AGnosis\AGnosis\config\drawn\sprites\` and
  `%APPDATA%\AGnosis\AGnosis\config\drawn\balloons\` — the `directories` crate
  repeats the application name on Windows, hence the two `agnosis` folders.
  The grass tiles live in `drawn\grass\`, the decorations in
  `drawn\decorations\` and the editor's drafts in `drawn\drafts\`.
- **Linux:** `~/.config/AGnosis/drawn/sprites/` and
  `~/.config/AGnosis/drawn/balloons/`
- **macOS:** `~/Library/Application Support/com.agnosis.agnosis/drawn/sprites/`
  and `~/Library/Application Support/com.agnosis.agnosis/drawn/balloons/`

### Putting a drawn asset into the parade

`agnosis list-drawn` prints the assets you have drawn; `agnosis assign <slot>
<name>` points a parade slot at one of them:

| Slot | What it replaces |
| --- | --- |
| `BIG1`, `BIG2` | the first and the second big duck |
| `SMALL1` … `SMALL9` | the nine trailing small ducks |
| `BALLOON1` … `BALLOON5` | the five balloons, in ring order |

`agnosis assign --clear SMALL3` puts the built-in art back. The same mapping
is the `[assignments]` table in `config.toml`. A drawn balloon can also be
summoned by name from the parade's `ADD BALLOON` line.

`agnosis assign` loads `config.toml`, changes the one slot and writes **only
the `[assignments]` table** back: every comment, every other table and any
key `agnosis` does not know survive untouched. The write is atomic and keeps
the previous file as `config.toml.bak`.

---

## Make It Yours

Everything below lives in `config.toml` — and it lands **live**. The parade
watches the file, so an edit is on screen within a second and no restart
is needed. A broken file never takes the parade down: the last good values
stay, the error is written to the trace log, and fixing the file applies
it.

### The sky

```toml
[sky]
enabled = true
time_of_day = 0.35      # 0.0 = midnight, 0.5 = noon
```

The sky is a vertical colour ramp behind the parade: a zenith colour at
the top of the frame, a horizon colour at the grass line, and dark earth
below. `time_of_day` runs night → dawn → day → dusk → night. The ramp is
a **pure function** of the value, so the same setting always renders the
same frame.

### A message of your own

```toml
[message]
text = "BRING YOUR OWN DUCKS"
position = "above_grass"   # above_grass | bottom_left | bottom_right
fade_seconds = 8.0
```

The message is upper-cased and drawn in the block font, which carries only
`A-Z 0-9 . : _ - / \` and space. An unsupported character is **dropped and
reported**, and the rest of the message still shows. The message fades out
after `fade_seconds` (a message that never leaves is a message you stop
seeing); `fade_seconds = 0` means "always".

### A grass band of your own

Draw a **grass tile** in the editor (the kind dropdown offers `BAL`,
`SPR` and `GRS`), save it, and point the config at it:

```toml
[grass]
tile = "meadow"
```

A tile is a *tile*, not a frame: at most 4 rows and 200 columns, and it
is repeated across the whole ground band. `agnosis grass meadow` sets the
key for you and `agnosis grass --clear` puts the built-in band back.

### A backdrop behind the ducks

Point the config at a picture and it becomes a **scrolling layer of ASCII
art** behind the parade:

```toml
[[backdrop]]
image = "hills.png"
cols = 160
rows = 18
```

The picture is converted to a tile of characters and repeated across the
screen. Put the file in `<config>/backdrops/` (the app creates the folder)
or give an absolute path.

Up to three backdrops can be stacked, and **each scrolls at its own
speed** — the far one drifts, the near one keeps up with the ducks. The
speeds are code constants, not config keys: you choose the art, the app
chooses the depth. The conversion happens on a **worker thread**, so a big
picture never makes the parade stutter, and a picture that is missing or
broken is simply skipped.

### The weather

```toml
[weather]
mode = "auto"        # off | auto | rain | snow | wind | lightning
intensity = 1.0      # 0.0 .. 2.0 — the particle density
seed = 1             # the deterministic seed
```

- **Rain** — blue waves fill the *gaps* of the ground band and move, and
  blue splashes appear around every duck's feet.
- **Snow** — the whole band turns white, and one seeded duck falls on its
  bum while **every duck behind it stops**. The order never changes and
  the stopped ducks keep walking in place: the rhythm is untouched.
- **Wind** — every `\` blade in the band leans the other way.
- **Lightning** — the tall blades turn yellow and the short ones navy
  blue for half a second, and the **whole parade stops for 1.5 seconds**.

`mode = "auto"` runs a fixed 60-second cycle of all of them. The weather
is a pure function of the clock and `seed`, so it is reproducible — the
same seed always produces the same rain.

### A personality per duck

```toml
[[personality]]
slot = "small_3"
bob = 1.5
gait_speed = 1.2
phase_offset = 2
hop = 0.5
color = [255, 120, 60]
```

A personality is a **delta**, not a replacement: the config lists only the
ducks that differ. `bob` is the bob amplitude in cells, `gait_speed` and
`phase_offset` change the **animation phase** — never the train's
spacing — and `color` repaints the duck. The size class is deliberately
*not* a personality: the train's geometry depends on it. The editor's
settings panel can write this block for you: its PERSONALITY `RANDOMISE`
action draws a delta per duck and `RESET` removes the block again.

### Coming back tomorrow

```toml
[stats]
show_days = true
```

The counters live in `<config>/stats.json`: the first parade's date, the
number of parades, the streak of consecutive days and the achievements
earned. The file is written atomically, and a broken one is treated as
"no stats" rather than as a failure. The achievements are **listed in a
file, not in the code** (`<config>/achievements.toml`), so you can add
one without a rebuild.

---

## Sound and Reactions

The parade can talk, and the ducks can notice you.

### Click a duck

The parade window is borderless and fullscreen, so the pointer maps
straight onto the grid. **Click a duck** and it reacts: it hops, its face
changes (it holds its walk frame and squashes as it lands), it flashes
white for a moment and it clucks.

**Shift-click** and the duck *stops in place* — and the ducks behind it
stop too, while the ducks in front keep marching. The train never loses
its order and never loses its rhythm: a stopped duck keeps walking on the
spot, because the gait is a function of the simulation's clock, not of the
train's position.

Clicking empty ground does nothing at all.

### The note track

The parade can also play a **MIDI note track** — a line of notes placed on
a strip, one cell per sprite step, so the tune moves at exactly the speed
the ducks march.

Open the editor with the **TRK** button (or the `Ctrl+Shift+D` chord):

- The **strip** is the canvas: the top row is the note lane, the bottom
  row is a ruler that marks the loop's start and its end. One step is one
  sprite frame, so the strip is as long as the crossing window (263 steps
  at the default settings).
- The **palette** on the right holds the twelve basic notes of an octave
  (`C3`..`B3`). Click a note to pick it, then click a lane cell to place
  it; click the cell again to take it away.
- The **name field** is the track's name, **SAVE** writes it to
  `<config>/tracks/<name>.json`, and **Enter** makes the parade play it
  (it writes the `[audio] track` key).
- `Ctrl+Z` / `Ctrl+Shift+Z` undo and redo a placed note.

The sequencer's clock is the **scene's own clock**, so a note cannot drift
away from the animation: a note in slot *k* fires on the sprite step *k*.

From the shell: `agnosis track list`, `agnosis track show <name>` and
`agnosis track use <name>`.

### The sound

Every sound is **synthesized** — no audio file is shipped and none is
loaded from disk. The synth is a small pure-Rust sine/triangle/square
oscillator with an ADSR envelope; the one-shots (the cluck, the balloon
squeak, the jump chirp, the rain splosh and the thunder) are generated
the same way. Each effect has its own gain and its own **cooldown**, so a
train of events cannot machine-gun the sound card.

```toml
[audio]
volume = 0.7
muted = false
track = "marching"
waveform = "triangle"
```

**No sound card is not an error.** The device is opened on a worker
thread; if there is none, the app says so in one trace line and the parade
carries on in silence. A machine with no audio (a CI runner) is a normal
case, and the exit code is never affected.

---

## Send It to Somebody

The parade is a pure function of a few numbers, so it can be **written
down** and sent. Two artifacts come out of the same recording.

### The self-contained page

```sh
agnosis export parade.html
```

One file that opens in Chrome, Firefox and Edge straight from a `file://`
URL, a chat app or a USB stick. **No network, no assets, no fonts**: the
frames, the glyphs and the config travel inside the file.

The page is a canvas, a blob decoder and a timer, and it can be **paused
and scrubbed** — the slider at the bottom walks the loop. It is not a
second implementation of the simulation: the frames are the app's own
frames, so the page cannot drift from what you saw.

At the default settings a 20 s loop at 12 fps is about **650 kB**.

### The animated GIF

```sh
agnosis export parade.gif
```

The same loop as a GIF: the ducks, the grass, the balloons and the HUD
are all readable at the exported size (960x540), and the file is about
**1.4 MB** for a 20 s loop.

### Both of them

- The export is **reproducible**: the same parade exports byte-identical
  files, every time.
- The export runs **off the UI thread**: the parade keeps marching while
  the file is written.
- `--fps` sets the frame rate (12 by default), `--frames` the number of
  frames (one loop by default) and `--scale` the GIF's size (2 by default:
  the app's own size is 1).

---

## The web build

The parade is not a program that draws ducks — it is a **library that
draws ducks**, and the native app is one front end over it. The web
build is the other: the same simulation, the same art, the same physics,
running in a browser tab. It is a second front end, not a port: the
simulation, the art and the exporters live in the `agnosis-core` package
and are shared byte for byte, so the two cannot drift apart.

### Build it

```bash
./crates/agnosis-web/build-web.sh crates/agnosis-web/dist
```

The script builds the module with cargo and then runs the **wasm-bindgen
CLI**, falling back to **wasm-pack** when the CLI is not installed. The
CLI's version must match the crate's (`0.2.129`), because the CLI
refuses a mismatched module. The output folder holds three files:

```
index.html         the page
agnosis_web.js       the wasm-bindgen glue
agnosis_web_bg.wasm  the module
```

### Serve it

Any static host serves them. There is **no** server-side component and
the page fetches **nothing** from the network:

```bash
npx http-server crates/agnosis-web/dist -p 8765
```

> **The page needs a static host — `file://` does not work.** An ES
> module cannot be imported from the file system (and the module is not
> served with the `application/wasm` content type), so the browser
> refuses the page. Open `http://localhost:8765/` instead of double
> clicking `index.html`.

### The three back ends

The page picks the fastest back end the browser has, and the top bar's
**backend** selector forces one:

| Back end | What it is |
| --- | --- |
| `webgpu` | the browser's WebGPU |
| `webgl2` | the browser's WebGL2 — the one every browser has |
| `canvas2d` | the plain 2D canvas: the RGBA frame is uploaded with `putImageData`, no GPU involved at all |
| `auto` | the default: WebGPU, then WebGL2, then the 2D canvas |

A forced back end the browser does not have falls back to `auto`, and
the status line says so. The status line shows the back end, the grid
size, whether the parade is running or paused, and how many drawn
assets the page carries.

The forced back end is also reachable **from the URL**, so you can pin
one without touching the page:

```
http://localhost:8765/?backend=webgl2
```

The parameter takes `auto`, `canvas2d`, `webgl2` or `webgpu` — the same
four the selector offers. The page waits for the parade to boot before
it applies the request, so the parameter is never lost, and the top
bar's selector shows the back end that is really in use.

### What the web build can and cannot do

| | |
| --- | --- |
| **The parade, the art, the balloons, the ropes** | **Yes** — it is the core |
| **The sky, the weather, the grass, the messages** | **Yes** — they are core content layers |
| **Mouse reactions** | **Yes** — and this is where the web is *better* than the native one: a click makes a duck jump, a shift-click stops it and the ducks behind it |
| **The drawn art** | **Yes**, read-only — a package or a manifest is imported through the file input |
| **The editor** | **No** — it stays native; the *art* travels, not the editor |
| **The sound** | **No** — the core still emits its audio events, but the web build has no audio device |
| **The global hotkey, the browser minimize, the folder watcher, the autostart** | **No** — the web *is* the browser, so the joke is the native app's |

### The art travels

The page's **art** file input takes two things:

- a **`.agnosis` package** — the file `agnosis export` writes (the drawn art,
  the assignments and the look), applied to the running parade;
- a bare **manifest** — the file `agnosis config export` writes.

The page reads the bytes, so the file's *name* does not matter: a ZIP
header means a package, anything else is read as a manifest. Both are
applied to the parade as it runs, so the ducks change without a reload.

---

## Configuration

Configuration is stored in `config.toml` in the platform-standard configuration directory:

- **Windows:** `%APPDATA%\AGnosis\AGnosis\config\config.toml`
  (e.g. `C:\Users\<User>\AppData\Roaming\AGnosis\AGnosis\config\config.toml`)
- **Linux:** `~/.config/AGnosis/config.toml`
- **macOS:** `~/Library/Application Support/com.agnosis.agnosis/config.toml`

The balloon sprite directory is located alongside config at `<config_dir>/balloons/`. Dropping images or `.ans` files into this directory is automatically detected at runtime via a directory watcher.

`monitor` picks the display the popup covers: `cursor` (the display under the pointer) or `primary`. The displays are enumerated per platform — Windows through `EnumDisplayMonitors` with a per-monitor DPI query, Linux through X11 RandR, macOS through `NSScreen` — and the pointer position is read per platform too, so `cursor` works on all three. When the Linux or macOS probe fails, `agnosis` falls back to a single 1920×1080 display and logs a warning. The macOS probe reads AppKit and therefore has to run on the main thread.

### Default Configuration (`config.toml`)

The block below is exactly what `agnosis` writes into `config.toml` on first
start — the file is generated by `Config::default()`, so it is reproduced
here byte for byte:

```toml
loop_seconds = 20.0
cell_size = [
    12,
    24,
]
monitor = "cursor"
max_cols = 4096
fps_cap = 30
exit_hint = "press ESC to exit"
hint_corner = "bottom-right"

[balloons]
max = 5
balloon_radius_rows = 36
rope_min_small = 55
rope_min_big = 85
rope_max = 150
pendulum_g = 9.8
pendulum_damp = 0.35
pendulum_drive = 0.9
pendulum_period = 1.6

[hotkey]
windows = "A+G+Shift+Space"
macos = "Ctrl+Alt+Shift+Space"
linux = "A+G+Shift+Space"
editor = "Ctrl+Shift+D"

[browsers]
names = []
restore_on_close = false

[assignments]

[message]
text = ""
position = "above_grass"
fade_seconds = 8.0

[sky]
enabled = false
time_of_day = 0.35

[grass]

# The parallax backdrops: pictures that scroll behind the ducks.
# The scroll speed is a code constant, not a key.
# [[backdrop]]
# image = "hills.png"    # a file in <config>/backdrops/, or an absolute path
# cols = 160             # the tile width in cells (the tile repeats)
# rows = 18              # the band height in cells

[weather]
mode = "off"
intensity = 1.0
seed = 1

[stats]
show_days = false

[decorations]

[audio]
volume = 0.7
muted = false
track = ""
waveform = "triangle"
strip_steps = 263
```

What the keys mean:

- `loop_seconds` — how long the train takes to cross the screen (the march
  speed is `cols / loop_seconds` cells per second).
- `cell_size` — **legacy**: the per-cell pixel size has been ignored since
  the row-budget grid landed; it is kept so old configs keep loading.
- `monitor` — `cursor` (the display under the mouse pointer) or `primary`.
- `max_cols` — a grid-width safety cap (a perf valve, not a layout control).
- `fps_cap` — the target render framerate.
- `exit_hint`, `hint_corner` — the on-screen exit hint and the corner it
  sits in (`bottom-left` or `bottom-right`).
- `[balloons]` — the balloon ring size (`max`, 5 slots), the balloon radius
  in grid rows (`balloon_radius_rows`; the column radius follows the cell
  aspect), the randomized rope bounds in rows (`rope_min_small`,
  `rope_min_big`, `rope_max`) and the pendulum physics (`pendulum_g`,
  `pendulum_damp`, `pendulum_drive`, `pendulum_period`).
- `[hotkey]` — the parade chord per platform (`windows`, `linux`, `macos`)
  and `editor`, the draw-your-own chord. `editor` is **one string for every
  platform**; an **empty value disables the editor chord**, and a value that
  does not parse is reported and dropped while the parade chord keeps
  working.
- `[browsers] names` — custom process names to minimize (case-insensitive,
  `.exe` stripped). When it is empty, the built-in browser list is used:
  chrome, chromium, msedge, firefox, waterfox, librewolf, seamonkey,
  brave, vivaldi, opera, yandex, thunderbird, tor, torbrowser, epiphany,
  konqueror, comodo, torch, thorium, iron, avast, avg.
- `[browsers] restore_on_close` — **put the minimized browsers back when
  the parade ends.** `false` (the default) keeps them minimized, which is
  the joke; `true` restores them automatically. The browsers are only ever
  **minimized**, never closed, so no work is lost either way. See HOW TO
  USE point 1.
- `[assignments]` — which drawn asset replaces which built-in art; see
  below.
- `[message]` — a **custom message** on the parade. `text` is upper-cased
  and restricted to the block font's characters (`A-Z 0-9 . : _ - / \` and
  space); an unsupported character is dropped and reported, and the rest
  of the message still shows. `position` is `above_grass` (centred, two
  rows above the grass — the default), `bottom_left` or `bottom_right`.
  The message is shown for `fade_seconds` and then fades out over one
  second; `fade_seconds = 0` means "always".
- `[sky]` — the **procedural sky gradient**. `enabled` turns it on;
  `time_of_day` is `0.0` (midnight) to `1.0` (midnight again, through
  dawn, noon and dusk). The ramp is a pure function of the value, so the
  same value always renders the same frame.
- `[grass]` — `tile` names a drawn grass tile (see *Make It Yours*). The
  tile is repeated across the whole ground band.
- `[weather]` — `mode` is `off` (never), `auto` (a fixed 60-second cycle
  of lightning, rain, clear, snow, wind, clear), or one forced effect
  (`rain`, `snow`, `wind`, `lightning`). `intensity` is `0.0` to `2.0` and
  scales the particle density. `seed` seeds the **deterministic**
  generator: the weather is a pure function of the clock and the seed, so
  the same seed always produces the same rain.
- `[stats]` — `show_days` draws the "days since the ducks" counter in the
  bottom-left corner. It is **off** by default; the counters live in
  `<config>/stats.json`.
- `[decorations]` — the drawn **decorations** the ducks wear: `big` names
  the one the two big ducks wear and `small` the one the nine small ducks
  wear. A decoration is drawn **over** the duck's body, and a name that
  no drawn asset carries leaves the ducks bare (the built-in art is
  untouched).
- `[audio]` — the **sound**. `volume` is the master gain (`0.0` to `1.0`),
  `muted` silences everything without losing the volume, `track` names the
  note track in `<config>/tracks/` the parade plays (empty means "no
  track"), `waveform` is the voice the notes are played in (`sine`,
  `triangle` or `square`) and `strip_steps` is the length of the note
  strip in sprite steps (263 at the default settings — one step is one
  sprite frame). See *Sound and Reactions* below.
- `[[personality]]` — one table per duck that differs from the built-in
  art. `slot` names the duck (`big_1`..`big_2`, `small_1`..`small_9`),
  `bob` is the bob amplitude in cells, `gait_speed` scales the
  **animation phase** (never the train's spacing), `phase_offset` shifts
  the phase by whole frames, `hop` is an extra hop in cells and `color`
  is an RGB triple. A slot that does not name a duck is dropped with a
  message.

The built-in list holds executable names and is matched exactly, so `mychrome` is not `chrome`. On macOS the built-in browsers are also matched by bundle id, which is how the Tor Browser (executable `Tor Browser`) is recognized; a custom list on macOS additionally matches a localized app name or a bundle-id tail.

#### The `[assignments]` table

Sixteen explicit keys, one per parade slot. A missing or empty value means
"no override", so the built-in art stays. The default file leaves the table
empty; it fills up as you assign things:

```toml
[assignments]
big_1 = "cat"       # the first big duck (train slot 0) shows the drawn sprite "cat"
small_1 = "dog"     # the first small duck (train slot 2) shows "dog"
```

Only the slots that carry an assignment are written: a slot with no
override is simply absent, and the built-in art stays.

`agnosis assign <slot> <name>` writes into this table, and
`agnosis assign --clear <slot>` takes the entry out again. A drawn balloon
can also be summoned by name from the parade's `ADD BALLOON` line.

The grid is **row-budget driven**: the cell size comes from the monitor height (`cell_h = monitor_h / 360`, `cell_w = 2/3 · cell_h`), so a duck covers the same share of the screen on every display and DPI. `cell_size` is legacy and ignored.

### Repaired configuration values

Out-of-range values are clamped when the config is loaded, every repair is
logged as a warning, and the repaired value is **written back into the file**
— so the warning appears once, not on every run. The write replaces the
repaired value in place, so your comments and any keys `agnosis` does not know
survive.

| Key | Out-of-range value | Repaired to |
| --- | --- | --- |
| `loop_seconds` | not finite, or `<= 0.1` | `20.0` |
| `max_cols` | `< 960` / `> 8192` | `4096` / `8192` |
| `fps_cap` | `0`, or `> 240` | `30` |
| `balloons.max` | outside `1..=5` | `5` |
| `balloons.balloon_radius_rows` | `0` / `> 128` | `36` / `128` |
| `balloons.rope_min_small` | `0` | `55` |
| `balloons.rope_min_big` | `< rope_min_small` | `rope_min_small` |
| `balloons.rope_max` | `< rope_min_big` | `rope_min_big` |
| `balloons.pendulum_g` | not finite, or `<= 0` | `9.8` |
| `balloons.pendulum_damp` | not finite, or `< 0` | `0.35` |
| `balloons.pendulum_drive` | not finite | `0.9` |
| `balloons.pendulum_period` | not finite, or `<= 0` | `1.6` |
| `assignments.<key>` | an empty or blank name | dropped — the slot keeps the built-in art |
| `assignments.<key>` | a name no drawn asset carries | dropped — the slot keeps the built-in art (the file may still show up later) |
| `assignments.<key>` | whitespace around the name | trimmed |
| `audio.volume` | not finite | `0.7` |
| `audio.volume` | outside `0.0..=1.0` | clamped into it |
| `audio.strip_steps` | `0` / `> 4096` | `1` / `4096` |
| `audio.track` | a blank name | dropped — the parade plays no track |
| `audio.waveform` | a name that is not `sine`, `triangle` or `square` | `triangle` |

The `[hotkey].editor` chord is **not** repaired here: an empty value simply
disables the editor chord, and a value that does not parse is reported by
the daemon and dropped, so the parade chord keeps working.

---

## Hotkey Chords per Platform

The default chord configurations are:

- **Windows:** `A+G+Shift+Space`  
  Monitored via a low-level keyboard hook (`WH_KEYBOARD_LL`). Key repeats are ignored, and re-arming requires releasing chord keys.
- **Linux (X11):** `A+G+Shift+Space`  
  Captured via `XRecord` (with passive `XQueryKeymap` polling fallback). Does not grab keys globally or interfere with other applications.
- **macOS:** `Ctrl+Alt+Shift+Space`  
  Captured with a **listen-only** `CGEventTap`: the tap observes the key events without swallowing them, so the permission macOS asks for is **Input Monitoring** and not Accessibility. Letters, digits, `Space`, `Esc` and `F1`–`F20` all work, and a chord may hold any number of them — but a chord that names a key no backend can produce (a punctuation mark, or `F21` and above) parses and then never fires.  
  The macOS **login item is the menu bar**, not the daemon: `agnosis install` writes a LaunchAgent that runs `agnosis menubar`, so the daemon — and with it this chord — is **not** started at login on macOS. Run `agnosis start` for the chord (the menu-bar trigger works without it, and without any grant).

There is a **second** chord, the same string on every platform:

- **Editor chord:** `Ctrl+Shift+D` (`[hotkey].editor`)  
  Opens the *Draw Your Own* editor, and closes it again when it is already open. It never minimizes your browsers. Set `editor = ""` to disable it.

On macOS the first run asks for the **Input Monitoring** grant:
**System Settings → Privacy & Security → Input Monitoring** → add *AGnosis* →
switch it on. Without the grant the daemon exits with
`hotkey unavailable: grant Input Monitoring to AGnosis in System Settings →
Privacy & Security → Input Monitoring`; `agnosis`, `agnosis editor` and the
menu-bar item stay reachable. A detached daemon has no console, so its
warnings (including that one) only reach the `AGNOSIS_TRACE_FILE` log file
when that environment variable is set.

---

## Install

### Windows — the installer (recommended)

Run `agnosis-setup.exe`: it asks **where to install** (any folder you like,
`%LOCALAPPDATA%\Programs\AGnosis` by default), shows the README during the
install, copies the exe + README + LICENSE, adds a Start-Menu shortcut, and
offers to register the **login autostart** and **start the daemon right
away** — the global hotkey (and with it the browser minimization) works as
soon as the daemon is up. The uninstaller reverses all of it. The installer
and how to build it live in [`packaging/windows/`](packaging/windows/).

### Other platforms and the portable path

Pre-built packages are published on every `v*` tag. Pick the one for your platform:

| Platform | Artifact | Install |
| --- | --- | --- |
| Windows (x86-64) | `agnosis-<version>-windows-x64.zip` | Unzip anywhere and run `agnosis.exe`. |
| macOS (Intel + Apple silicon) | `agnosis-<version>-macos-universal.dmg` | Mount the image and drag `agnosis.app` into `/Applications`. |
| Linux (Debian/Ubuntu, x86-64) | `agnosis_<version>_amd64.deb` | `sudo dpkg -i agnosis_<version>_amd64.deb` — `agnosis` lands on `PATH` and shows up in the application menu. |

### What the portable exe can do without the daemon

The daemon owns exactly two things: the **global hotkey** and the browser
minimization that goes with it. Everything else runs from a bare exe — unzip,
double-click, and the fullscreen parade starts:

- **`F2`** opens the *Draw Your Own* editor: draw walking sprites, balloons,
  grass tiles and decorations, save them, and point parade slots at them with
  the editor's **ASSIGN** panel — the running parade picks the change up live.
- **`Ctrl+O`** opens the balloons folder; drop a picture in and the parade
  carries it, or type its name on the input line and press `Enter`.
- **`F1`** shows the full README; **`F12`** copies the live frame to the
  clipboard; a click pokes a duck, shift-click holds it.
- **Export** works from a terminal next to the exe, no daemon involved:
  `agnosis export loop.html` (self-contained page), `agnosis export loop.gif`
  (animated GIF), or `agnosis export loop.agnosis` (a package of your art and
  settings). `agnosis add picture.png` copies new balloon art into the folder
  the same way.

The only thing a portable run cannot do is answer a hotkey — there is no
keyboard hook without the daemon, so the parade is opened (and closed with
`ESC`) by hand.

Everything the packages contain, how they are built, and how to verify them
locally is documented in [`packaging/README.md`](packaging/README.md).

### Per-platform caveats

- **Windows — the binary is a GUI-subsystem binary.** It opens no console
  window, so a double-clicked `agnosis.exe` never flashes a black console next
  to the duck parade. The CLI verbs **do** print when they are run from a
  terminal — a GUI process inherits the parent's standard handles — so
  `agnosis.exe list-drawn` prints its list and `agnosis.exe menubar` prints its
  error. Only the output is invisible when the binary is double-clicked or
  spawned detached by the daemon (whose stdio is nulled). The headless
  selftest writes files: `agnosis.exe selftest --frames 2 --out out` writes
  `out/frame_0000.png`, `out/frame_0001.png`, … — assert on those PNG files
  as well as on the output.
- **macOS — the global hotkey needs the Input Monitoring grant.** macOS only
  lets an app read the keyboard after the user allows it explicitly:
  **System Settings → Privacy & Security → Input Monitoring** → add *AGnosis* →
  switch it on. The grant is tied to the bundle id `com.agnosis.daemon`, so it
  survives an upgrade of the app. The **menu-bar item** is the permission-free
  path: it starts the parade — and the *Draw Your Own* editor — without asking
  for any grant, so it keeps working when the grant is missing or revoked.
  Without the grant the daemon exits with the exact remediation text (see
  *Hotkey Chords per Platform*).
- **macOS — the app is unsigned and un-notarised.** Notarisation needs an
  Apple developer account, so Gatekeeper blocks the first launch: right-click
  `agnosis.app` → **Open**, or allow it once in **System Settings → Privacy &
  Security** (**Open Anyway**).
- **Linux — the X11 runtime libraries are required**, and a Wayland-only
  session needs **XWayland** installed (`sudo apt install xwayland`) with
  `DISPLAY` set: the binary is built against winit's X11 backend, so the
  popup opens through the XWayland X server. The `.deb` names the libraries in
  its `Depends:` line, and the release CI checks that line twice: against the
  **link-time** closure `ldd` reports, and against the hand-written list of
  the X11 libraries winit `dlopen`s at runtime (`ldd` never reports those,
  so the second check is what catches a package that installs but will not
  start).

---

## Repository layout

The project is a Cargo **workspace** with three packages:

| Package | Path | What it is |
| --- | --- | --- |
| `agnosis` | the repository root (`src/`) | the **shell**: the CLI, the daemon, the popup window, the editor window, the platform tree, the folder watcher, the GPU renderer, the audio device |
| `agnosis-core` | `crates/agnosis-core/` | the **pure core**: the simulation, the art, the scene, the data model, the exporters, the editor's pure state, the audio's pure side. Platform-free by construction — it names no OS and no platform tree, and it compiles for `wasm32-unknown-unknown` |
| `agnosis-web` | `crates/agnosis-web/` | the **web front end**: the same parade in a browser tab (WASM) |

The rule the split enforces: the core never resolves a path or a
platform default. The shell resolves them (`src/host.rs`) and hands the
core plain data, so the core can be compiled for a platform the app was
never built for.

---

## Building and Installation

### Prerequisites

- **Rust:** Rust 1.90 or newer (Rust 2021 edition)
- **Cargo**
- **Linux dependencies (Ubuntu/Debian):**
  ```bash
  sudo apt-get install -y libx11-dev libxext-dev libxfixes-dev libasound2-dev
  ```
  `libasound2-dev` is the ALSA header: the parade can play a note track,
  so the binary links against ALSA. The **runtime** library
  (`libasound2`) is what the `.deb` depends on; the headers are only
  needed to build.

### Build and Run

To build the project in release mode:

```bash
cargo build --release
```

That builds the native binary (`agnosis`). The other two packages have
their own commands:

```bash
cargo build -p agnosis-core   # the core library
```

and the web crate is built for the browser by the script below.

To run the test suite:

```bash
cargo test --workspace
```

> `cargo test` on its own runs **only the root package** (the shell) —
> the core's and the web crate's tests are not in it. Use
> `cargo test --workspace` for the whole suite.

To run a headless selftest (no display or GPU needed):

```bash
cargo run -p agnosis -- selftest --frames 2 --out ./selftest_out
```

To build the web front end (see *The web build* above):

```bash
./crates/agnosis-web/build-web.sh crates/agnosis-web/dist
```

To type-check the **browser** build — the same command the CI runs:

```bash
rustup target add wasm32-unknown-unknown
cargo check -p agnosis-core -p agnosis-web --target wasm32-unknown-unknown
```

To install and launch the resident daemon:

```bash
agnosis install
agnosis start
```

---

## License & Assets

- Built with Rust and licensed under the **Open Source License with Ethical
  Restrictions** in [`LICENSE.txt`](LICENSE.txt) — an open-source license with
  ethical restrictions and a reserved name ("AGnosis"): use, modification and
  distribution are free for any purpose that honours the human-rights terms,
  and modified forks must rename. Contact the licensor for alternative
  licensing.
- Bundled font: **DejaVu Sans Mono** (licensed under the Bitstream Vera License, notice included in `crates/agnosis-core/assets/fonts/LICENSE-BitstreamVera.txt`).

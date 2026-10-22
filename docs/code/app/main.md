# `app` — the `krkr` CLI and the emulator entry point

Source: `crates/app/src/main.rs` (the binary), `crates/app/src/runner.rs`
(the entry point and the platform bridge)

Two files make up the launch path. `main.rs` is the **desktop launcher**: it
parses the command line, reads the configuration file, resolves the game
path and installs the process logger. `runner.rs` is the **emulator entry
point**: it takes the finished `Launch` and runs it, offscreen or in a
window.

The split is the point: the emulator core performs no configuration I/O and
knows nothing about the command line. Another platform's launcher resolves
its own configuration and calls the same `app::run`.

---

## 1. The command line

```
krkr [--headless] [--frames N] [--fps N] [--backend null|vulkan]
     [--preload-archives] [--debug-overlay] [--graphic-cache-limit LIMIT]
     [--archive XP3 ...] [--base-dir DIR] [GAME]
```

| option | value | default | effect |
|---|---|---|---|
| `--headless` | — | off | run the offscreen frame loop; needs neither a display nor a GPU |
| `--frames N` | frames | 120 | how many frames the headless loop steps |
| `--fps N` | `1..=1000` | 60 | the frame-rate limit; paces the windowed loop and the headless step |
| `--backend` | `null`, `vulkan` (`vk`) | `null` | the render backend to create |
| `--preload-archives` | — | off | read every mounted archive into memory in the background |
| `--debug-overlay` | — | off | draw the debug panel in the top-left corner |
| `--graphic-cache-limit` | `none`, `unlimited`, `0`, bytes | the reference default (disabled) | the decoded-image cache policy |
| `--archive XP3` | path | — | mount an archive; repeatable, order-significant, later wins |
| `--base-dir DIR` | path | the first archive's parent | the app path; every relative archive path resolves against it |
| `GAME` | path | — | a game directory (becomes the app path) or an XP3 file (is mounted) |

`-h` / `--help` prints the usage table. The table is generated from the same
`OPTIONS` constant the parser uses, and a test walks it, so an option cannot
be added without help text.

**Parsing is total.** Every value is parsed and validated before anything
takes effect, so a malformed command line has no partial effect. A parse or
configuration failure prints the message plus the usage and exits with code
`2`; a failed run exits with code `1`.

`--backend vulkan` needs a real window with a display connection (Wayland /
X11 / XCB); the windowed host passes both the window and the display handle
so a Vulkan surface can be created. The headless path has no display and
therefore uses the null backend.

---

## 2. The game path

`resolve_game` mirrors the reference's discovery from its first command-line
argument:

* a **positional directory** becomes the app path;
* a **positional file** is mounted as an archive;
* `--base-dir` and a positional directory are mutually exclusive;
* the app path is `--base-dir` (or the positional directory) when given,
  otherwise the parent of the first archive, otherwise the working
  directory;
* every relative archive path resolves against the app path, so
  `--base-dir /game --archive data.xp3` mounts `/game/data.xp3`;
* no game input at all is not an error: it is the empty-scene run.

The existence of the base directory is not checked here — a missing one is
reported when the storage mounts.

Loading rules — the explicit list's authority, the directory scan, the
precedence — are in [`../../migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md);
the reference behaviour behind the positional argument is in the
[evidence trail](../../../docs.old/migration/09-game-loading.md).

---

## 3. Configuration and logging

`load_desktop_config` reads the `key=value` file named by the
`KRKR_CONFIG` environment variable. An unset variable means no
configuration; a configured file that cannot be read is a **typed error**,
never an empty result.

The merge precedence is **launch argument > configuration file > built-in
default**, applied field by field, and it is implemented in
`resolve_runtime_config`. Both sources have already been validated, so the
merge cannot fail. The value types and the file grammar are in
[config.md](config.md).

`init_logger` installs the process logger — the one place in the tree where
a `log` backend is installed — with `RUST_LOG` selecting the filter and
`info` as the default. A second installation is a no-op, so an embedding
launcher may install its own logger first.

---

## 4. `runner` — the entry point

```rust
pub struct Launch {
    pub runtime: RuntimeConfig,          // the validated configuration
    pub backend: BackendKind,            // the render backend to create
    pub game: Option<GameInput>,         // the game to open, or None
    pub mode: RunMode,                   // offscreen or windowed
}

pub enum RunMode {
    Headless { frames: u32 },
    Windowed,
}

pub struct GameInput {
    pub base_dir: PathBuf,               // the app path
    pub archives: Vec<PathBuf>,          // in mount order, later wins
}

pub struct RunReport { pub last_frame: Option<StepReport> }

pub fn run(launch: Launch) -> Result<RunReport, String>
```

`run` dispatches on the mode and returns after the headless loop finishes or
after the windowed event loop exits.

### The headless loop

`run_headless` builds a headless `HostConfig` (null backend, the requested
overlay and cache policy), opens a `GameHost` when the launch names a game,
and steps `frames` frames with `dt_ms` derived from the frame-rate limit.
It prints the null backend's counters and the scene summary — the window
count, the layer count, the bitmap count and, per window, how many layers
the window's root list can actually reach. That last number is the
reachability the reference's draw path uses, so a scene with many unreached
layers renders as an empty window and the summary says so.

### The windowed loop

`run_windowed` builds a `PlatformConfig` (the window title `krkr-rs`, the
frame interval from the frame-rate limit) and hands a `DesktopApp` to
`platform::run`. The platform loop owns the window and the ticker; the
handler owns the host.

`DesktopApp` is the bridge from L1 platform events to the L2 host:

| callback | what it does |
|---|---|
| `resumed` | takes the window and display handles — a missing handle is fatal, never replaced by a synthetic one — creates the `Host` at the window's **real physical size**, and opens the `GameHost` when a game is present |
| `event` | `Resized` queues the newest surface size and the VM's `onResize` (the reference posts `onResize`); the size is applied once at the next frame boundary, before the frame body draws; `ScaleFactorChanged` needs no action, because winit follows it with a `Resized`; input and focus events are queued and delivered with the next frame; `CloseRequested` exits |
| `frame` | delivers the queued events, steps the host, applies a script-requested window resize, and logs the heartbeat |

An **OS window resize** is applied at the frame boundary, with the newest size
the event loop reported. A compositor can report several sizes for one layout
change (a fractional-scale tiling rounds the height back and forth); applying
them as they arrive rebuilds a swapchain for each, and a rebuild nothing is
presented from cannot show the frame. One size per frame is applied instead.

The **script-requested window resize** is the other direction of the resize
story: a script's `Window.setInnerSize` makes the engine produce a request,
`Host::take_window_resize` takes it (so a size that does not change again
is applied once), and the handler asks the OS window to adopt it. A backend
that applies the size synchronously emits no `Resized`, so the surface is
resized from the size the backend reports back. When the compositor keeps the
final say on the size (a tiled window), the size it settles on arrives as the
following `Resized` event, which the next frame applies.

The **heartbeat** is one `debug` line per second of delivered frames, with
the frame index, the frame gap and whether the frame reached the surface.
The platform loop logs the ticker deadline for the same second, so an alive
loop that presents nothing is distinguishable from one that stopped
delivering frames.

---

## 5. Related documents

| topic | document |
|---|---|
| the crate overview and the wiring | [README.md](README.md) |
| the game host, the mount and the startup sequence | [game.md](game.md) |
| the configuration types and the file grammar | [config.md](config.md) |
| the platform loop and the event contract | [`../arch/l1.md`](../../arch/l1.md) |
| game loading and startup discovery | [`../../migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
| the runtime configuration contract | [`../../runtime-config.md`](../../../docs.old/runtime-config.md) |

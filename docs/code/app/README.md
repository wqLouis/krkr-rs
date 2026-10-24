# `app` — the desktop composition root

Source: `crates/app/` · crate `app` · layer **root**

`app` is the composition root: it selects the platform backend, builds the
services, attaches the L2 state to the L3 surface, binds the L4 VM host and
runs the loop. It holds **no engine logic, no game semantics and no platform
logic** — only the wiring that decides which service talks to which, in which
order, and how a failure is reported.

The crate is one of two roots: `crates/app` (desktop, this crate) and
`crates/android` (the Android `GameActivity` root). Both drive the *same*
`app::Host` / `app::GameHost` through the same `platform::EventHandler`, so
the difference between them is the launcher and the event source, never the
emulator. See [`../arch/overall.md`](../../arch/overall.md) §2 for the crate
map and [`../rules/arch_rules.md`](../../rules/arch_rules.md) §12 for the
backend selection rules.

---

## 1. Boundaries

| | |
|---|---|
| Depends on | `api` (L1 DTOs + config), `fs`, `media`, `render`, `platform` (L1), `engine` (L2), `compat` (L3), `vm` (L4) |
| May name | every layer — it is a root, and the wiring is its job |
| Must not contain | engine rules, platform rules, TVP vocabulary, game-specific branches |
| Lint | `#![deny(unsafe_code)]` at the crate root; `#![warn(missing_docs)]` |

The crate root denies `unsafe`. The three places that need raw pointers —
the attachment of the L2 service handles to the L3 surface, the compat
context lookup in the frame tail, and the storage address the video source
reads through — carry a local `#[allow(unsafe_code)]` with a `SAFETY:` note
that states the ordering invariant the pointer depends on. Everything else
is safe Rust.

The backend is chosen **per target**, not at run time: `Cargo.toml` pulls
`platform` with `platform-linux` and `media` with `video-ffmpeg` on Linux,
and `platform-android` / `video-mediacodec` on Android. The desktop crate
therefore contains no `#[cfg(target_os = …)]` of its own.

---

## 2. Module map

| module | source | one-line role | document |
|---|---|---|---|
| crate root | `src/lib.rs` | `Host`, `HostConfig`, `HostError`, the crate's public surface | this page |
| `main` | `src/main.rs` | the `krkr` CLI: arguments, the configuration file, the game path | [main.md](main.md) |
| `runner` | `src/runner.rs` | `run`, `Launch`, `RunMode`, the headless loop, the windowed event bridge | [main.md](main.md) |
| `game` | `src/game.rs` | `GameHost`: the mount, the service attachment, the startup sequence, the frame hook | [game.md](game.md) |
| `audio` | `src/audio.rs` | the L1 decoder seam and the L1 output device the frame drains | [audio.md](audio.md) |
| `video` | `src/video.rs` | the L1-backed video source installed on the `VideoOverlay` and movie seams | [video.md](video.md) |
| `overlay` | `src/overlay.rs` | the render-only debug panel (FPS, CPU, I/O, memory, object counts) | [overlay.md](overlay.md) |
| `image_codec` | `src/image_codec.rs` | the L1 image codec installed on `Bitmap.load` / `loadHeader` / `save` | [game.md](game.md) §4 |
| `window_ex` | `src/window_ex.rs` | the emulated Windows host facts (monitors, metrics, environment) | [game.md](game.md) §4 |
| `startup` | `src/startup.rs` | the start-up cost counters the installed seams record | [game.md](game.md) §2 |

---

## 3. The two hosts

`Host` is the assembled **engine host**: the L2 `Engine` and the frame it
owns. It has no frame state of its own; `Host::step` drives exactly one
engine frame. The stateless L1 services are created where they are used — the
pipeline and the surface by `Host::new`, the file system and the media
backend by `GameHost::open_with_preload` — so the host holds no copy of a
service it does not exercise.

`GameHost` is the **game host**: `Host` plus the L4 `VmHost`, the mounted
`Storage`, the L1 audio device and the ordered frame body. It is the
counterpart of the empty scene, and it is what a launcher builds when a game
is present.

```
launcher
  └─ app::run(Launch { runtime, backend, game, mode })
       ├─ RunMode::Headless { frames } → Host (+ GameHost when a game is named)
       └─ RunMode::Windowed          → platform::run(DesktopApp)
```

The windowed path never calls `Host::step` directly: `DesktopApp` is a
`platform::EventHandler`, and the platform loop owns the window and the
ticker. See [main.md](main.md) §4.

---

## 4. Public surface

```rust
// construction and frames
pub struct Host;                          // the engine host
pub struct HostConfig;                    // #[non_exhaustive]; default() and headless()
pub enum HostError { Render(..), Config(String), Overlay(String), Frame(String) }
pub type StepReport = engine::FrameReport;

pub struct GameHost;                      // the game host
pub enum GameError { Mount(..), Storage(..), Startup{..}, NoStartup, Host(..),
                     Audio(..), Vm(..), Plugin(..) }

// the emulator entry point
pub struct Launch { runtime, backend, game, mode }
pub enum RunMode { Headless { frames: u32 }, Windowed }
pub struct GameInput { base_dir: PathBuf, archives: Vec<PathBuf> }
pub struct RunReport { last_frame: Option<StepReport> }
pub enum RunError { Host(HostError), Game(GameError), Platform(PlatformError) }
pub fn run(launch: Launch) -> Result<RunReport, RunError>;

// the one seam a launcher may install itself
pub fn install_image_codec(host: &CompatHost, media: &Media, storage: &Storage);

// re-exported from L1
pub use render::BackendKind;
```

The audio decoder, the image codec, the video source and the emulated
Windows host facts are installed by `GameHost::open` with the shared
start-up counters; only the image codec is public, because a launcher that
embeds the emulator may own its own storage. `run` reports a typed
`RunError`, so the host, the game and the platform failures stay
distinguishable instead of being flattened into a message.

---

## 5. Wiring

```
             ┌────────────────────────── app (this crate) ───────────────────────────┐
             │                                                                       │
  L1 fs ─────┤ FileSystem::host(base_dir)  ──▶ Storage (L2)                           │
  L1 media ──┤ Media::new()                ──▶ image codec / audio decoder / video   │
  L1 render ─┤ RenderPipeline + Surface     ──▶ Engine (L2)                          │
  L1 platform┤ platform::run(DesktopApp)    ──▶ the event loop and the ticker        │
             │                                                                       │
  L2 engine ─┤ Engine::step(dt, hooks)      ◀── the frame; the hook is VmHook        │
             │                                                                       │
  L3 compat ─┤ CompatHost::install(engine)  ──▶ the script-visible surface           │
             │                                                                       │
  L4 vm ─────┤ VmHost::tick(FrameCtx)       ──▶ the frame body                      │
             └───────────────────────────────────────────────────────────────────────┘
```

Three rules govern the wiring, and each of them is a decision the rest of the
tree depends on:

1. **The launcher owns configuration.** `app::run` takes a finished
   `RuntimeConfig`; the emulator core reads no file, no environment variable
   and no platform store. See [config.md](config.md).
2. **The core owns the frame.** Nothing above L2 owns a `dt`: the platform
   loop measures it, the root passes it to `Engine::step`, and the engine
   owns the clock, the timer pump, the projection and the single draw call.
   See [`../arch/l2.md`](../../arch/l2.md) §5.
3. **The root owns the input mapping.** Pointer and touch positions arrive
   in device pixels; the root maps them back into VM pixels through the L2
   translation before the VM sees them, so an L3 hit-test never leaves VM
   space. See [game.md](game.md) §5.

---

## 6. Failure modes

Every failure the root can produce is a **typed error**, never a degraded
substitute:

| error | raised when |
|---|---|
| `HostError::Config` | a headless host was asked for a real backend, or a windowed host has no window handle |
| `HostError::Render` | the backend could not be created, or a surface action failed |
| `HostError::Overlay` | the debug panel could not be built or uploaded |
| `HostError::Frame` | the frame hook failed, so the frame was abandoned before drawing |
| `GameError::Mount` | an explicitly named archive could not be mounted |
| `GameError::Storage` | a storage probe failed while resolving the startup sequence |
| `GameError::Startup` / `GameError::NoStartup` | the startup script failed and no fallback exists, or no startup script was found |
| `GameError::Audio` | the L1 audio device could not be opened or written |
| `GameError::Vm` | the VM could not start or a frame body failed |
| `GameError::Plugin` | auto-loading a `.tpm` plugin failed |

Two failures are deliberately **not** fatal and each announces itself: the
scanned archive that cannot be read is skipped with a warning (a stray file in
a game folder cannot stop an otherwise playable mount), and a `patch.tjs` or
`AfterStartup.tjs` that fails is reported and the startup continues. An
*explicitly named* archive is never skipped.

---

## 7. Tests

| file | covers |
|---|---|
| `tests/host.rs` | the empty-scene host: the cache policy, the texture upload, the resize, the projection cadence, the window-resize request, the desktop rectangle, the inert clipboard, a self-updating transition |
| `tests/debug_overlay.rs` | the overlay node: present only when enabled, uploaded before it is drawn |
| `tests/frame_timing.rs` | the slow-frame attribution: the phase that dominates, and the wall-clock total that contains the sum |
| `tests/image_codec.rs` | `Bitmap.save` through the installed codec, and the resolution of a lowercased absolute name to the real path |
| `tests/real_game.rs` | the mounted archives and the decoded assets of a real title |
| `tests/boot_game.rs` | booting a real title end to end — **ignored**; it needs `KRKR_TEST_GAME` |
| `tests/common/mod.rs` | `headless_host`, `build_solid_and_bitmap_scene` |

The unit tests inside the modules cover the argument parser and the
configuration precedence (`main.rs`), the slow-frame line and the `.tpm`
scan and the project-path choice (`game.rs`), the chunk arithmetic
(`audio.rs`), the overlay formatting (`overlay.rs`), the desktop facts
(`window_ex.rs`) and the counter snapshot (`startup.rs`).

The tests that bind the process-global VM serialize on a mutex: the VM keeps
process-global state, so two engines may not run at once. `testkit::vm_lock`
is the shared helper, and `testkit::game_archives` derives the archive list
a real-game test mounts.

---

## 8. Related documents

| topic | document |
|---|---|
| the layers, the crate map, the coordinate spaces | [`../arch/overall.md`](../../arch/overall.md) |
| the frame contract, the composition root's place in it | [`../rules/arch_rules.md`](../../rules/arch_rules.md) §4, [`../arch/l2.md`](../../arch/l2.md) §5 |
| the L3 ↔ L2 seam the root attaches through | [`../rules/api_rules.md`](../../rules/api_rules.md), [`../migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
| game loading and the startup order | [`../migration/09-game-loading.md`](../../../docs.old/migration/09-game-loading.md) |
| the VM host and the frame body | [vm.md](../vm.md), [`../../migration/08-tjs-vm-and-native-plugins.md`](../../../docs.old/migration/08-tjs-vm-and-native-plugins.md) §6 |
| runtime configuration | [config.md](config.md), [`../runtime-config.md`](../../../docs.old/runtime-config.md) |
| the Android root | [`../android.md`](../../../docs.old/android.md) |
| the working rules for agents and humans | [`../../../AGENTS.md`](../../../AGENTS.md) |

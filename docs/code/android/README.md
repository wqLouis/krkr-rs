# The Android composition root (`crates/android`)

Source: `crates/android/src/` · crate `android` (cdylib `krkr_android`) ·
layer **root** (composition root)

`crates/app` is the desktop composition root and must not branch on the
operating system, and `platform` is L1: it may not call up into the emulator.
The OS entry point is therefore its own crate per host. This crate owns
`android_main`, the `GameActivity` event loop and the JNI reads that hand the
launcher's choice to the shared host; it contains **no engine logic**. The
desktop binary (`krkr`) and this library drive the same `app::Host` and
`app::GameHost` through the same `platform::EventHandler`.

The full design — the launcher, the storage decision, the touch bridge, the
MediaCodec backend and the packaging — is in
[`android.md`](../../../docs.old/android.md) (evidence trail:
[`docs.old/android.md`](../../../docs.old/android.md)).

---

## 1. The modules

| module | source | role | document |
|---|---|---|---|
| `android_entry` | `src/lib.rs` | the `android_main` entry point: the startup order, the logger install, the settings hand-off | [host.md](host.md) |
| `host` | `src/host.rs` | the `GameActivity` event loop and the `AndroidHost` handler | [host.md](host.md) |
| `state` | `src/state.rs` | the `krkr_state.json` startup-state file the launcher polls | [host.md](host.md) §5 |
| `log_file` | `src/log_file.rs` | the capped, rotated `krkr.log` the launcher's Logs screen reads | [host.md](host.md) §6 |
| `input` | `src/input.rs` | the Android input events → the platform-neutral `PlatformEvent`s | [input.md](input.md) |
| `gesture` | `src/gesture.rs` | the touch → mouse synthesizer (the state machine) | [gesture.md](gesture.md) |
| `config` | `src/config.rs` | the launcher's settings → the validated `RuntimeConfig` | [config.md](config.md) |
| `jni_fields` | `src/jni_fields.rs` | the static-field JNI reads | [config.md](config.md) §4 |
| — | `build-android.sh` | the cross-compile, the staging and the verification | [build.md](build.md) |
| — | `check-jni-libs.sh` | the packaging check: the dependencies and the page alignment of the staged libraries | [build.md](build.md) §6 |

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | root (composition root) — wiring only |
| Host-only dependencies | `log`, `api` |
| Android-only dependencies (`cfg(target_os = "android")`) | `android-activity` (the `game-activity` feature), `jni`, `app` (the shared host), `platform` (the `platform-android` feature), `env_logger` |
| Must not depend on | `engine`'s internals, `compat`'s internals, `vm`'s internals — it reaches them through `app` |
| Forbidden | engine logic, game semantics, a `#[cfg]` that branches on the *runtime* OS |

The crate type is `cdylib` and the library name is `krkr_android`, which must
match `android.app.lib_name` in the manifest — that is how the AGDK
`GameActivity` finds the native entry point.

Everything that touches Android is target-scoped, so on a desktop host the
crate compiles to just the (host-tested) state, log-file, config, gesture and
JNI-field modules, and the desktop dependency graph is unchanged.

---

## 3. The startup order

`android_main` runs a fixed order, and each step exists because the next one
depends on it:

1. **Read the state directory** (`KrkrGameActivity.pendingStateDir`). The
   state file and the logger are then available for every later failure. A
   read failure is not the same as an unset field, but with no directory there
   is nowhere to write the state file, so stderr is the only channel here.
2. **Read and validate the log level** (`pendingLogLevel`). This is the one
   settings read that happens *before* the logger exists, because the level
   selects the filter. An unknown level is echoed on stderr and written to
   the state file as a failure; the loop is never started.
3. **Install the logger.** The file is opened *before* the global logger is
   claimed: an unusable log file is a reported startup failure, never a
   fallback that leaves the launcher's Logs screen empty.
4. **Write `starting`** to the state file.
5. **Read the game directory** (`pendingGameDir`). A missing directory is a
   startup failure.
6. **Read the game name** (`pendingGameName`). It only decorates a log line,
   so a read failure is logged rather than fatal.
7. **Read the remaining settings** (`pendingFrameRate`,
   `pendingPreloadArchives`, `pendingDebugOverlay`) and resolve them into the
   `RuntimeConfig`.
8. **Create the handler and run the loop.** The loop's outcome is written to
   the state file at the end: a recorded failure, or a clean stop. A later
   `stopped` must never erase a failure.

---

## 4. The loop

`host::run_loop` drives `android-activity`'s `GameActivity`:

* the lifecycle events (`InitWindow`, `TerminateWindow`, `WindowResized`,
  `ConfigChanged`, `Resume`, `Pause`, `Destroy`, `SaveState`, `LowMemory`)
  are normalized into the same `PlatformEvent`s the desktop backend emits;
* the frames are paced by a `platform::Ticker` and rendered at the deadline;
  the Android `RedrawNeeded` and content-rect callbacks also request a frame,
  so the first frame and a surface change are presented promptly;
* while paused the ticker keeps running but **no frame is stepped**;
* the poll and the input dispatch time are accumulated and reported to the
  handler as the `FramePacing` of the next frame.

The handler is `AndroidHost`, which implements `platform::EventHandler` — the
same trait the Linux loop calls, so the engine above the platform boundary
cannot tell the two hosts apart.

---

## 5. What the emulator core does not do here

The engine core performs no configuration I/O on Android, exactly as on the
desktop. The launcher owns loading configuration; the emulator receives the
finished value:

| setting | how it reaches the engine |
|---|---|
| the game directory | `GameHost::open_with_preload(host, Vec::new(), game_dir, preload_archives)` — a directory-only launch mounts every `*.xp3` under the folder |
| the archive preload | the same `open_with_preload` argument |
| the debug overlay | `HostConfig::with_debug_overlay` |
| the frame rate | the loop's ticker period |
| the log level | the process logger's filter |

The launcher persists its choices in its own settings file and publishes them
over JNI; the emulator reads and validates them but never writes them back.

---

## 6. The unverified-on-host limitation

**The Android target is never built or tested on the development host.** There
is no NDK and none may be installed, so:

* the `platform-android` feature compiles the platform-independent helpers
  plus a typed stub on the host; everything that needs the NDK stays gated to
  `target_os = "android"`;
* `uv run --project scripts krkr-gate android-target` resolves the dependency
  graph per target **without compiling**, and fails when a desktop-only
  backend feature leaks into the Android target or when two mutually exclusive
  backends are enabled together;
* the real Android target is compiled and run on a machine that has an NDK.

Everything Android-only is therefore documented as **unverified**, never as
working. The pieces that a device must still show are listed in
[build.md](build.md) §7 and in [`android.md`](../../../docs.old/android.md) §8.

What *is* host-tested: the state-file writer, the log-file sink, the settings
resolver, the JNI field names, and the whole touch → mouse state machine.

---

## 7. Related documents

| topic | document |
|---|---|
| the Android design, the launcher, the storage decision | [`android.md`](../../../docs.old/android.md) · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the event loop and the handler | [host.md](host.md) |
| the input translation | [input.md](input.md) |
| the touch → mouse bridge | [gesture.md](gesture.md) |
| the settings resolution | [config.md](config.md) |
| the cross-compile and the packaging | [build.md](build.md) |
| the Android backends (AAudio, MediaCodec, `AHardwareBuffer`) | [`10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) · snapshot [`docs.old/migration/10-android-backends.md`](../../../docs.old/migration/10-android-backends.md) |
| the L1 Android window and audio backend | [`../platform/platform.md`](../platform/platform.md) |
| the MediaCodec video backend | [`../platform/media.md`](../platform/media.md) §6 |
| the layer rules and the composition-root rule | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §2, §4.2 |
| the Rust-level checklist | [`../../rules/code_styles.md`](../../rules/code_styles.md) |

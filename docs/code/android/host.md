# `host` — the Android event loop, the entry point and the diagnostics files

Source: `crates/android/src/lib.rs` (the `android_entry` module),
`crates/android/src/host.rs`, `crates/android/src/state.rs`,
`crates/android/src/log_file.rs`

The `android_main` entry point, the `GameActivity` loop and the two files the
launcher reads back.

---

## 1. Responsibility

* `android_main` — the native entry point the AGDK `GameActivity` resolves:
  the startup order, the logger install and the settings hand-off.
* `AndroidHost` — the `platform::EventHandler` bound to one `GameActivity`
  surface; it creates the render host, opens the game, queues the input and
  the focus events, and drives one frame.
* `run_loop` — the `android-activity` loop: the lifecycle events, the input
  queue, the ticker and the frame pacing.
* `state` — the `krkr_state.json` startup-state file the launcher polls.
* `log_file` — the capped, rotated `krkr.log` the launcher's Logs screen
  reads.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | root (composition root) |
| Depends on | `app` (the shared `Host`/`GameHost`), `platform` (the `platform-android` feature), `android-activity`, `jni`, `env_logger`, `log`, `api` |
| Must not depend on | `engine`'s internals, `compat`'s internals, `vm`'s internals |
| Forbidden | engine logic, game semantics, a stub or an approximation of an engine behaviour |
| Lint | `#![deny(unsafe_code)]`, `#![warn(missing_docs)]` |

The `state`, `log_file`, `config` and `gesture` modules are plain `std` and
name no Android type, so the **host test target** compiles and exercises them;
the Android-only modules are target-gated.

`unsafe_code` is denied at the crate root. The one exception is the JNI
module, which carries a scoped `#[allow(unsafe_code)]` — the `jni` crate's raw
pointer handling requires it, and every block carries a `SAFETY:` note.

---

## 3. The entry point

```rust
#[unsafe(no_mangle)]
pub fn android_main(app: AndroidApp);
```

The symbol is resolved by the `GameActivity` glue, which the manifest names
through `android.app.lib_name`. It uses the Rust ABI because the glue
declares `extern "Rust" { fn android_main(app: AndroidApp); }`.

The startup order is in [README.md](README.md) §3. Two details are worth
restating here:

* **The log level is read before the logger exists**, because it selects the
  filter. A read failure or an unknown level is echoed on stderr *and*
  written to the state file — which is what the launcher reads — and the loop
  is never started.
* **The log file is opened before the global logger is claimed.** An unusable
  log file is a reported startup failure rather than a fallback that leaves
  the Logs screen empty.

### The logger

One global logger fans every record out to two sinks:

* `env_logger` on stderr, which `android-activity` redirects to logcat. The
  filter is `RUST_LOG` when it is set, otherwise the launcher's log level;
* `FileSink` on `<filesDir>/krkr.log`, the file the launcher's Logs screen
  reads.

The records are filtered **once**, by the `env_logger` filter, so the file
holds exactly what stderr holds. `RUST_LOG` keeps its precedence for anyone
who can set it; a device cannot set it, so the launcher's level is what a
device uses. `info` omits the VM's diagnostic detail — the VM's per-exception
disassembly, register dump and console output reach the logger as `debug`
records, so **`debug` is the level that surfaces a script exception** in
`krkr.log`.

Installing the global logger twice can only mean that an earlier `android_main`
in this same process already installed it: `android-activity` calls
`android_main` once per Activity instance and the launcher and the engine
share the process. The existing logger already fans every record into the
same `krkr.log`, so a second launch keeps logging instead of failing.

---

## 4. The handler

```rust
pub(crate) struct AndroidHost {
    backend: BackendKind,          // the render backend the composition root asked for
    game_dir: PathBuf,             // the folder the launcher selected
    preload_archives: bool,
    debug_overlay: bool,
    running: Option<GameHost>,     // the game, created by `resumed`
    pending_events: Vec<PlatformEvent>,
    gestures: GestureSynthesizer,
    gesture_clock: Instant,
    outcome: state::RunOutcome,     // the first fatal error, if any
}
```

`AndroidHost` mirrors the desktop host: the L2 render host is created once
the `ANativeWindow` exists and is torn down before that window is destroyed.
Android always launches a game, so there is no empty-scene variant.

| callback | what it does |
|---|---|
| `resumed(window)` | obtains the window and the display handle, creates the `app::Host` with the Vulkan backend and the surface size, then opens the game with `GameHost::open_with_preload` |
| `event(event)` | queues the input and the focus events; a resize resizes the engine's framebuffer and queues `PlatformEvent::Resized`; `CloseRequested` exits |
| `frame(dt_ms, window, pacing)` | takes the pending events and steps the game with `game.step_with_pacing(dt_ms, &events, pacing)` |

A missing window or display handle is **fatal**, never replaced by a
synthetic one.

The resize path resizes the surface only; the VM learns about the new size as
a script event, like the reference window posting `onResize`. A failed resize
queues nothing, so the VM never sees a size the surface does not have.

---

## 5. The state file

The launcher and the engine share one process, but the engine runs native code
on its own thread: when a game fails to start, or a native panic kills the
process, the Kotlin side is not guaranteed to run afterwards — and a surface
with no content is indistinguishable from a slow load. The state file gives
the launcher a small on-disk signal it can poll instead.

The contract with the launcher is fixed:

* directory `<filesDir>`, passed to the native code as
  `KrkrGameActivity.pendingStateDir`;
* file name `krkr_state.json`;
* a flat JSON object:

```json
{ "state": "starting", "message": null, "pid": 1234, "at": "2026-09-15T12:00:00Z" }
```

`state` is one of `starting`, `running`, `failed`, `stopped`; `message`
carries the error text for `failed` (otherwise `null`); `pid` is the process
id; `at` is an RFC 3339 UTC timestamp.

**The file is not cleared on entry.** A process that dies by panic or
`SIGKILL` leaves the last state it wrote behind — `starting` or `running` —
which is exactly how the launcher tells a crash apart from a normal exit
(which writes `stopped`).

The write is **atomic**: the record is written to a temp file in the same
directory, flushed to the backing store (`sync_all`), and then renamed over
the destination. The rename is what makes the replace atomic; without the
fsync, a power loss could leave the previous `starting`/`running` visible
after the new state was reported, which the launcher would read as a crash. A
failed create/write/sync/rename leaves the previous state intact and never
panics.

A failure to write the state file is logged and swallowed: the state file is
a debugging aid and must not itself become a startup failure. On a host
build (no state directory configured) `write_state` is a no-op.

`RunOutcome` accumulates the run's outcome: a failure recorded anywhere in
the run is terminal, so the clean `stopped` an ordinary exit writes cannot
replace a failure the launcher has to report.

---

## 6. The log file

`krkr.log` is the file the launcher's Logs screen and its crash dialog read.
The launcher reads the last 400 lines, up to 256 KiB from the end, and drops
a leading partial line.

* The line format is `"<rfc3339> <LEVEL> <message>"`. A multi-line message is
  written verbatim; only the first line carries the timestamp.
* The file is capped at **1 MiB** and rotated through `krkr.log.1` …
  `krkr.log.6`: each rotation shifts the existing files up one slot and drops
  the oldest. The sink therefore holds seven files, about **7 MiB** worst
  case.
* The cap spans launches: `FileSink::open` adopts the current on-disk size,
  so a file left behind by a previous process is rotated rather than grown
  without bound.
* A record larger than the whole cap is still written whole — the file is
  never truncated mid-record — so a single oversized record adds its own size
  once rather than being silently dropped.
* A failed rename during the rotation is reported to stderr and the next slot
  is still shifted, so one unreadable file cannot stop the current log from
  being preserved.

---

## 7. State ownership

| value | owner |
|---|---|
| the render host and the running game | `AndroidHost.running` (a `GameHost`) |
| the pending input and focus events | `AndroidHost.pending_events` |
| the gesture state machine | `AndroidHost.gestures` |
| the run's outcome | `AndroidHost.outcome` |
| the state directory | one `OnceLock` set once at startup, so a second Activity in the same process cannot redirect the file |
| the engine state, the VM, the storage | L2/L4 (`app::GameHost`) |

---

## 8. Platform gating

| module | gate |
|---|---|
| `android_entry`, `host`, `input` | `cfg(target_os = "android")` only |
| `config`, `gesture`, `jni_fields`, `log_file`, `state` | `cfg(any(target_os = "android", test))` — they compile and are tested on the host |

`jni_fields` keeps its field-name constants outside the gate, so a host unit
test can pin the names `KrkrGameActivity` publishes; only the read itself is
Android-only.

---

## 9. Failure modes

| failure | behaviour |
|---|---|
| the state directory cannot be read | reported on stderr; the loop continues (there is nowhere to write the state file anyway) |
| the log level is unknown or unparsable | echoed on stderr, written to the state file as `failed`, and the loop is **not** started |
| the log file cannot be opened | a reported startup failure (the state file says `failed`); never a fallback to stderr only |
| the game directory is unset or unreadable | a reported startup failure; the loop is not started |
| the game name cannot be read | logged as a warning; the name is blank, which only affects a log line |
| the window or the display handle cannot be obtained | fatal: the run ends and the state file says `failed` |
| the render host cannot be created | fatal |
| the game cannot be opened | fatal |
| a frame fails | fatal: the first fatal error is recorded and the state file reports it |
| the state file cannot be written | logged and swallowed (the file is a debugging aid) |
| the log file cannot be opened or written | reported to stderr once (the logger cannot log its own failure without recursing) |

A fatal error is **reported**, never swallowed and never replaced by a
fallback.

---

## 10. Tests

* `crates/android/src/state.rs` `mod tests` — the JSON shape, the RFC 3339
  timestamp, the atomic replace, the missing-directory path and the
  `RunOutcome` precedence (a failure is terminal).
* `crates/android/src/log_file.rs` `mod tests` — the record format, the byte
  accounting, the multi-step rotation (the slot order, the dropped oldest, a
  previous process's file rotated rather than appended, and the cap holding
  for every file), reopening a previous file and the missing-directory error.
* `crates/android/src/config.rs` `mod tests` and
  `crates/android/src/jni_fields.rs` `mod tests` — see
  [config.md](config.md) §7.

**Not verified on the host:** that `<filesDir>` is writable by the app
process, that the records appear in `adb logcat` under the `env_logger`
format, that the Logs screen and the crash dialog read them back, and that
the engine starts, decodes and renders on a real phone.

---

## 11. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the input translation the handler consumes | [input.md](input.md) |
| the touch → mouse state machine the handler owns | [gesture.md](gesture.md) |
| the settings the entry point resolves | [config.md](config.md) |
| the Android design and the launcher contract | [`android.md`](../../../docs.old/android.md) · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the L1 window and the Android backend | [`../platform/platform.md`](../platform/platform.md) |
| the composition-root rule | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §4.2 |

# Android JNI seam audit — Kotlin launcher ↔ Rust engine

Read-only audit of the seam between `android/app/src/main/kotlin/dev/krkr/rs/`
and `crates/android/src/`, checked against `docs/android.md`,
`docs/runtime-config.md`, `android/README.md`, `crates/platform/api/src/config.rs`,
`crates/compat/src/event.rs`, the platform backends and the `android-activity`
0.6.1 contract. No cargo/gradle was run (host builds only; the Android target
is never built on this machine). Everything below was verified by reading both
sides; items marked **[device]** cannot be confirmed without hardware.

Findings are ranked: crash/black-screen > wrong behaviour > dead code/docs.

---

## Findings, ranked

### F1 (severity: launch-blocking) — a second game launch in the same process always fails: the logger is claimed once and treated as fatal

`android-activity` calls `android_main` **once per Activity instance, in the
same process**: "if your `Activity` is destroyed and re-created … these
entrypoints may be called multiple times, for each `Activity` instance"
(`android-activity-0.6.1/src/lib.rs:68-77`). The launcher and the engine
Activity share one process (no `android:process` in
`android/app/src/main/AndroidManifest.xml`), and the normal flow is
`MainActivity` → `KrkrGameActivity` → back (Destroy) → `MainActivity`, with the
process kept alive. The next launch creates a new `KrkrGameActivity`, which
runs `android_main` again.

On the second entry, `init_logging` fails deterministically:
`log::set_boxed_logger(...).is_err()` →
`std::io::Error::new(AlreadyExists, "a global logger is already installed")`
(`crates/android/src/lib.rs:219-224`). `android_main` then writes
`failed` — "cannot start the engine log: a global logger is already installed"
— and returns without starting the loop (`crates/android/src/lib.rs:92-97`).
The launcher shows "The game could not start" with that message
(`MainActivity.kt:206-213`). The game cannot be launched a second time unless
Android kills the process in between.

Note `state::set_state_dir` is a `OnceLock` whose "first call wins" is
harmless here (same `filesDir`), so the logger claim is the only blocker.
The fix direction (for the main agent): treat `AlreadyExists` as success, or
make the fanout logger reusable across `android_main` entries. **[device]**
only to reproduce; the code path is deterministic from the crate docs.

### F2 (severity: startup failures reported as clean exit) — the unconditional trailing `stopped` overwrites every in-loop `failed`

`android_main` writes `Stopped` unconditionally after `run_loop` returns
(`crates/android/src/lib.rs:173`). Two failure paths inside the loop write
`Failed` first and then exit the loop, so `Stopped` lands on top of them:

* `AndroidHost::resumed`: `GameHost::open_with_preload` error → `Failed` with
  the message → `Flow::Exit` (`crates/android/src/host.rs:176-180`). This is
  the most likely real-world failure (audio device, archive mount, VM
  bootstrap, startup sequence — `crates/app/src/game.rs:159-186`).
* `AndroidHost::frame`: `game.step` error → `Failed` → `Flow::Exit`
  (`crates/android/src/host.rs:221-226`).

The launcher treats `stopped` as a normal exit and **clears the pending
launch marker**, showing nothing (`MainActivity.kt:231-236`, `else ->` branch
documented at `MainActivity.kt:229-230`). The user gets a black flash and back
to the launcher with no dialog; the reason survives only in `krkr.log`.
`state.rs`'s own doc says `Failed` is for "startup did not succeed"
(`crates/android/src/state.rs:59-61`) — the write happens but is clobbered
milliseconds later. Fix direction: make the terminal write conditional (e.g.
track the last state, or have `run_loop`/`EventHandler` return the terminal
state to `android_main`).

### F3 (same class as F2) — five fatal loop exits write no state at all, and are reported as clean `stopped` via F2's trailing write

All of these end the loop and then hit `crates/android/src/lib.rs:173`:

* window/display handle error in `resumed` (`crates/android/src/host.rs:137-149`);
* `Host::new` (Vulkan) error in `resumed` (`crates/android/src/host.rs:157-162`);
* `create_window` error on `InitWindow` (`crates/android/src/host.rs:267-271`);
* resize failure in `event` (`crates/android/src/host.rs:189-195`);
* `sync_script_size` failure after a frame (`crates/android/src/host.rs:357-360`).

These only log (`krkr.log`), and the launcher then reports a clean exit. The
desktop twin (`crates/app/src/runner.rs:372-429`) has the same exit pattern but
there stderr/exit-code is the reporting channel, so the omission is
Android-specific in effect. Vulkan-init failure on a device would be exactly
this path → black surface, no dialog. **[device]** to see which OEM drivers hit
it.

### F4 (wrong behaviour, cosmetic today) — `running` is never written on successful startup

`State::Running` is documented as "storage is mounted, natives are registered
and the VM is bootstrapped" (`crates/android/src/state.rs:52-54`), but the
only write is inside `MainEvent::SaveState` (`crates/android/src/host.rs:329-331`),
which Android sends when it snapshots the task — typically only when the game
is backgrounded. During normal play `krkr_state.json` stays `starting`.
Harmless today because the launcher treats `starting`/`running` identically
(`MainActivity.kt:215-227`), but any future consumer that trusts the doc will
be wrong. A `Running` write after `resumed` succeeds (or after `GameHost::open`)
would make the file match the contract.

### F5 (wrong behaviour, **[device]**) — surface recreation restarts the game from scratch

`MainEvent::InitWindow` always calls `handler.resumed`, which creates a fresh
`GameHost::open_with_preload` (`crates/android/src/host.rs:258-267`, `301-313`);
`TerminateWindow` only drops the running game (`host.rs:273-276`). A
`TerminateWindow` → `InitWindow` cycle (backgrounding, surface loss, some OEM
lifecycles) therefore remounts the archives and re-runs `startup.tjs`; all
in-memory progress since the last save is lost, silently. Whether the cycle
actually occurs with AGDK `GameActivity` needs a device. The VM has no
suspend/resume, so any fix is a design decision, not a patch — flagging, not
prescribing.

### F6 (D11 edge) — a JNI *read failure* on the three settings fields is indistinguishable from "unset" and silently applies defaults

`activity_static_string` returns `None` both when the field is null/empty and
when a JNI step fails (`crates/android/src/jni_fields.rs:69-91`; failures are
logged via `report`). For `pendingGameDir` this is fine — the `None` becomes a
typed `Failed` (`crates/android/src/lib.rs:104-112`). For
`pendingFrameRate`/`pendingScaling`/`pendingPreloadArchives` a failed read
(`crates/android/src/lib.rs:126-141`) flows into `resolve_runtime_config` as
"no override" and the default applies (`crates/android/src/config.rs:56-63`).
This is a fallback on an error path where the module promises "a present value
is never silently defaulted" (D11 as stated in `config.rs:11-16`) — the value
was present, the read failed. Practically the failure is logged to
`krkr.log`, so it is not invisible; severity is low. Fix direction: distinguish
"null/empty" from "read error" and make the latter fatal like the game dir.

### F7 (cosmetic) — `onNewIntent` re-points the statics at a new game while the engine still runs the old one

With `launchMode="singleTask"` (`AndroidManifest.xml`, `KrkrGameActivity`
declaration), launching game B while game A runs calls
`KrkrGameActivity.onNewIntent` (`KrkrGameActivity.kt:139-150`), which refreshes
`pendingGameDir`/`pendingGameName` and the settings fields — read values the
running `android_main` already consumed. Consequences: if game A later crashes,
`MainActivity.reconcilePendingLaunch` reports it under **game B's** name and
path (the marker was written for B at `MainActivity.kt:773`); and the statics
no longer describe the running engine. The native side ignores re-reads, so
nothing breaks in the engine. The code comments acknowledge the refresh; the
marker-naming mismatch is not mentioned anywhere.

### F8 (documented divergence, flagging for completeness) — drag slop is fixed 16 physical px; the reference scales it by DPI

`gesture.rs:41-47` uses `DRAG_SLOP_PX = 16.0`; the reference computes
`(Device::getDPI() / 10)²` (`reference/cpp/core/environ/cocos2d/MainScene.cpp:494-496`
— verified present), i.e. ~32 px at 320 dpi. On a typical high-dpi phone our
drag resolves about twice as early as the reference's. This is a deliberate,
documented divergence (`docs/android.md` §5, "The 16 px slop is a fixed
physical-pixel value"), consistent with the module's "no display service"
constraint, but it is a D28-adjacent deviation that a reviewer should keep on
the books. `LONG_PRESS_MS = 1000` matches the reference exactly
(`MainScene.cpp:1097-1101` — verified).

### F9 (docs drift) — `docs/runtime-config.md` Android snippet is stale

The snippet shows `config::resolve(stored_frame_rate, stored_scaling)` handing
a `Launch` to `app::run` (`docs/runtime-config.md`, §"crates/android" code
block). The actual API is `config::resolve_runtime_config(frame_rate, scaling,
preload_archives)` returning `RuntimeConfig`
(`crates/android/src/config.rs:48-52`), and `crates/android` drives
`host::run_loop` directly, not `app::run`. Two-argument `resolve` no longer
exists. Cosmetic but exactly the kind of doc the audit protocol says wins over
code — here the code is right and the doc is behind.

### F10 (hygiene) — duplicated option vocabulary between Kotlin and Rust with no drift guard

`RuntimeOptions` mirrors `api::config` as hand-copied constants
(`RuntimeSettings.kt:36-64` vs `crates/platform/api/src/config.rs:21-27`,
`120-131`, `174-183`). **Verified identical today** (see §3 below), but the
Kotlin app is not built by this workspace (`android/README.md`), so nothing
fails if one side changes. The comments say "mirrors" and rely on review. A
comment in `RuntimeSettings.kt` is the only guard; consider a host-side test
that greps the Kotlin constants against the Rust ones, or accept the risk
explicitly.

---

## 1. Field names and signatures — verified, no mismatch

The Rust side never spells a class name: it resolves the class from the
Activity instance (`env.get_object_class(&activity)`,
`crates/android/src/jni_fields.rs:59-63`), deliberately avoiding
`FindClass`'s class-loader pitfall on native threads (documented at
`jni_fields.rs:46-49`). `@JvmField` in a Kotlin `companion object` puts a real
**static field on the enclosing class** (`dev.krkr.rs.KrkrGameActivity`), which
is exactly what `GetStaticFieldID` on the instance's class finds — the
mechanism the Kotlin doc comment describes (`KrkrGameActivity.kt:36-43`).

| Rust constant (name, signature) | Rust read site | Kotlin `@JvmField` | Declared | Set before `super.onCreate`? |
|---|---|---|---|---|
| `FIELD_GAME_DIR` = `"pendingGameDir"`, `Ljava/lang/String;` | `lib.rs:102-103` | `pendingGameDir: String?` | `KrkrGameActivity.kt:47-48` | yes, `:114` |
| `FIELD_GAME_NAME` = `"pendingGameName"`, `Ljava/lang/String;` | `lib.rs:116-117` | `pendingGameName: String?` | `:51-52` | yes, `:115` |
| `FIELD_STATE_DIR` = `"pendingStateDir"`, `Ljava/lang/String;` | `lib.rs:83-84` | `pendingStateDir: String?` | `:67-68` | yes, `:116` |
| `FIELD_FRAME_RATE` = `"pendingFrameRate"`, `Ljava/lang/String;` | `lib.rs:129-130` | `pendingFrameRate: String?` | `:81-82` | yes, `publishRuntimeSettings()` at `:117` → `:158` |
| `FIELD_SCALING` = `"pendingScaling"`, `Ljava/lang/String;` | `lib.rs:134-135` | `pendingScaling: String?` | `:91-92` | yes, `:159` |
| `FIELD_PRELOAD_ARCHIVES` = `"pendingPreloadArchives"`, `Ljava/lang/String;` | `lib.rs:139-140` | `pendingPreloadArchives: String?` | `:101-102` | yes, `:160` |

All six names, types (`String` → `Ljava/lang/String;`) and the declaring class
agree. No unused field on either side: every Kotlin `@JvmField` is read, every
Rust constant is used. The ordering guarantee holds: all six fields are set in
`onCreate` before `super.onCreate(savedInstanceState)`
(`KrkrGameActivity.kt:114-121`), and `super` is what enters `android_main`
(`GameActivity` glue, `android.app.lib_name` = `krkr_android`,
`AndroidManifest.xml`; the cdylib name matches
`crates/android/Cargo.toml` `[lib] name = "krkr_android"`). `onDestroy`
(`KrkrGameActivity.kt:175-179`) clears only `pendingGameDir`/`pendingGameName`
and deliberately keeps `pendingStateDir`, matching the shutdown-write rationale
in its doc comment (`:59-63`). Empty-string values are treated as unset
(`jni_fields.rs:82-84`), which for the game dir becomes the correct typed
`failed` ("pendingGameDir is unset"), and for settings falls back to defaults
(see F6).

## 2. State and log contracts — verified compatible, with F2/F3/F4 as the deltas

**`krkr_state.json`.** Writer: `crates/android/src/state.rs`; reader:
`EngineDiagnostics.readState` (`EngineDiagnostics.kt:80-96`). Keys match:
`state`/`message`/`pid`/`at` (`state.rs:163-170` vs `EngineDiagnostics.kt:85-93`).
Vocabulary matches: `starting`/`running`/`failed`/`stopped` (`state.rs:70-73`;
Kotlin branches at `MainActivity.kt:206`, `:215`, `:231`). Null handling
matches: Rust emits `"message": null` for non-failed states; Kotlin checks
`isNull("message")` before `optString` (`EngineDiagnostics.kt:89-90`) and
treats a blank/`"null"` `state` as "no information" (`:85-86`). `pid` is
`std::process::id()` (`state.rs:167`), same process as the launcher, so the
"engine still alive" comparison `state.pid == Process.myPid()`
(`MainActivity.kt:217-221`) is sound. Writes are atomic (temp + rename,
`state.rs:135-159`) with fsync, so the reader never sees a half-object.

**Write ordering vs reconcile.** `markLaunch` writes the marker before
`startActivity` (`MainActivity.kt:773-778`); `android_main` writes `starting`
after `super.onCreate` enters it (`lib.rs:98`). The launcher's
"engine did not start" branch keys on `state == null || stateAge < markerAge`
(`MainActivity.kt:195-204`) — correct signature for a pre-native crash (JVM
crash before `android_main`, native lib load failure): the stale file from a
previous run has an older mtime than this launch's marker. Same filesystem,
same clock; no race found.

**Exiting without a terminal state.** By design a panic or SIGKILL leaves
`starting`/`running`, which the launcher reports as "stopped unexpectedly"
(`MainActivity.kt:215-227`) — intended, documented on both sides
(`state.rs:30-36`, `LauncherLog.kt:110-113`, `EngineDiagnostics.kt:28-34`).
The **false-negative** direction is the real defect: F2/F3 turn genuine
failures into a clean `stopped`, which also deletes the marker and thus the
evidence. One residual edge: a clean exit writes `stopped` *after*
`MainEvent::Destroy` (`lib.rs:173`); if Android SIGKILLs the process in that
millisecond window, a clean exit could be misreported as a crash — tiny window,
noted for completeness. A clean process kill (user swipes away) correctly
reports as unexpected stop, which is honest.

**`krkr.log`.** Writer: `crates/android/src/log_file.rs`; reader:
`LauncherLog.tail` (`LauncherLog.kt:127-147`). Format `"<rfc3339> <LEVEL>
<message>"` (`log_file.rs:180-188`), UTF-8, `\n` line endings, oldest first —
all of which `tail` assumes (split on `'\n'`, decode UTF-8, drop a leading
partial line when the read starts mid-file, `takeLast(400)` for the Logs
screen at `MainActivity.kt:71` / 25 for dialogs at `:75`). Bounded read
(256 KiB) vs the Rust 1 MiB rotation cap with one `krkr.log.1`
(`log_file.rs:46-51`): compatible — the tail always fits the cap. Rotation by
rename is atomic for a concurrent reader. Multi-line messages carry the
timestamp only on the first line (`log_file.rs:177-179`) — cosmetic, matches
the documented contract. The launcher reads but never writes `krkr.log`
(`LauncherLog.kt:22-27`); both sides agree on ownership.

## 3. Config precedence — ranges, defaults and names identical; the `fit` divergence is consistent

| Option | Kotlin (`RuntimeSettings.kt`) | Rust (`api::config`) | Resolved default (`config.rs:53-55`) |
|---|---|---|---|
| frame rate | `MIN 1` `MAX 1000` `DEFAULT 60` (`:37-43`) | `MIN_FRAME_RATE 1`, `MAX_FRAME_RATE 1000`, `DEFAULT_FRAME_RATE 60` (`config.rs:21-27`) | 60 when unset (`config.rs:58-61`) |
| scaling | `["game","fit","fill","expand"]`, `DEFAULT_SCALING "fit"` (`:49-58`) | `parse`: exactly those four (`config.rs:152-163`) | `Fit` when unset (`config.rs:53-55`, `64-67`) |
| preload | `["true","false"]`, default `"false"` (`:61-64`) | `parse_bool`: exactly `true`/`false` (`config.rs:240-249`) | `false` when unset (`config.rs:70-73`) |

Error-text ranges agree too: the engine error contains `1..=1000`
(`config.rs:36`, asserted by `crates/android/src/config.rs` tests) and
`game, fit, fill, expand`; the UI validates with the same constants
(`MainActivity.kt` SettingsScreen, `RuntimeOptions.FRAME_RATE_RANGE`). A
hand-edited invalid value survives the Kotlin store verbatim
(`RuntimeSettings.kt:168-178`) and is rejected by the engine with the option
name — surfaced via the `failed` state (F2 notwithstanding). No UI→engine or
engine→UI value mismatch found.

**`fit` vs `game`:** the Android default is `Fit` only inside
`crates/android`'s resolver (`config.rs:53-55`) via
`..RuntimeConfig::default()`; the shared built-in default stays
`PresentationScaling::Game` (`api/src/config.rs:126-131`, `207-215`). The
desktop path is unaffected: `crates/app/src/main.rs:298-306` resolves
`RuntimeConfig::default()` (scaling `game`), and `crates/android`'s config and
gesture modules are `#[cfg(any(target_os = "android", test))]`
(`crates/android/src/lib.rs:31-45`) so they never link into the desktop
binary. Verified on both sides; matches `docs/android.md` §2.2 and §3.2.

**Missing knobs / the doc's claim.** `RuntimeConfig` has exactly three fields
(`api/src/config.rs:185-201`) and the UI exposes all three. The claim in
`docs/android.md` §3.2 ("no control for texture resolution or audio
buffering, because the engine exposes no knob for either") is **true today**:
the renderer advertises only a hardware `max_texture_dimension`
(`crates/platform/render/src/lib.rs:136-138`), and audio pre-fill/cap are
`pub(crate)` constants (`PREFILL_MS = 150`, `MAX_BUFFER_MS = 2000`,
`crates/platform/platform/src/audio_queue.rs:53,62`), consistent with
`docs/runtime-config.md` §3.

## 4. Input seam — same event kinds as the desktop path where the platform can produce them; no TODOs or dead branches

There are no `TODO`/`FIXME`/`todo!` markers in `crates/android/src` or the
Kotlin sources (verified by grep). The wildcard arms in
`crates/android/src/input.rs:45-50` (Text/IME events, unknown motion actions)
are deliberate and documented, not dead branches.

**Event-kind parity** (Android `input.rs` vs desktop winit
`crates/platform/platform/src/linux.rs:253-273`):

| Kind | Desktop | Android | Notes |
|---|---|---|---|
| `PointerMoved` | `CursorMoved` | hover-move (`input.rs:90-97`) or gesture-synthesized | same physical-pixel space; both inverse-mapped through `app::map_input_to_logical` (`host.rs:227-232`, twin at `runner.rs:457-462`) |
| `MouseButton` | `MouseInput` | `ButtonPress/Release` via `mouse_button_from_android` (`input.rs:98-108`; table at `platform/platform/src/android.rs:203-213`) | 5-button mapping identical in shape to winit's |
| `Key` | `KeyboardInput` | `KeyAction::Down/Up` via `physical_key_from_android` (`input.rs:121-141`; 100+ entry table `android.rs:179-308`, `AKEYCODE_BACK → Escape` per `MainScene.cpp:820-829`) | unmapped codes keep raw identity as `PhysicalKey::Native` (`android.rs:180-187`) — no guessing (D11) |
| `Touch` | winit touch | **consumed** by the gesture synthesizer | deliberate: KAG menus read the mouse family; forwarding both would double-fire (`docs/android.md` §5, `gesture.rs:1-15`). Reference behaviour (`MainScene.cpp:1017-1146`) is virtual-mouse, so this matches the *reference Android* environment; a touchscreen Linux desktop diverges, which is consistent with "each platform's backend mirrors its reference environment" |
| `Wheel` | `MouseWheel` | **dropped** (`ACTION_SCROLL` hits the `_ => {}` arm, `input.rs:107`) | the reference's cocos2d Android environment contains no wheel/scroll handling at all (grepped `reference/cpp/core/environ/cocos2d/` — zero hits), so dropping it matches the reference Android build; only the desktop path offers a wheel |
| `PointerLeft` | `CursorLeft` | never | no cursor-leave concept on Android; window-level `onMouseLeave` only, no gameplay impact |
| `Text` (IME) | `Ime::Commit` | not translated (documented, `input.rs:44-46`) | phones type into system IMEs outside the game surface today |

**Gesture mapping plausibility.** Every synthesized path updates the cursor
before the button event (tap: move→press→release, `gesture.rs:253-263`; drag
resolve: move→press, `:222-232`; long press: move→right-press, `:135-148`),
which the compat layer requires: `MouseButton` carries no coordinates and
`EventAdapter` reads the cached pointer (`crates/compat/src/event.rs:145-157`,
`cache_input_state` at `:104-107`). Coordinates are `ANativeWindow` physical
pixels, the same space as `PlatformWindow::size()` (`android.rs:82-88`), and
are inverse-mapped to logical before hit-testing, exactly like the desktop
(`compat/src/event.rs:145-151` consumes the already-logical values). Two-finger
tap/swipe-down emit the same Escape press/release pair as `AKEYCODE_BACK`.
Multi-touch bookkeeping (primary/others counting, cancel releases a held
button, a second finger cannot hijack an active drag) is coherent and
host-tested (`gesture.rs` test module, 12 tests). Slop/DPI divergence is F8;
long-press matches the reference's 1000 ms exactly. One **[device]** item:
`gesture_tick` runs while the loop is paused (`host.rs:345-349` — it is called
whenever `!exit`), so a hold that spans a Pause could in principle fire a
right-press on the elapsed time if Android delivers neither `ACTION_CANCEL` nor
`ACTION_UP` before pausing; normally Android sends the cancel, so this is a
confirm-on-hardware item, not a defect found in the code.

## 5. Error surfacing — the JNI path is panic-free; the gaps are F1-F3 and F6

* **No unwrap/expect on the JNI path.** `jni_fields.rs` handles every JNI step
  with typed matches, logs, and clears the pending Java exception
  (`report`, `jni_fields.rs:95-103`). The only `unwrap_or_default` is the
  game *display name* (`lib.rs:117-119`), used for a log line only — the
  mount path never depends on it. The two remaining `expect`s in non-test code
  are guarded: `gesture.rs:231` is preceded by the same-`Option` check on the
  previous line, and `log_file.rs`'s `unwrap_or(0)` on `metadata()` (two sites)
  degrades only rotation accounting, not a write. No `#[allow(dead_code)]` or
  `#[allow(unused)]` anywhere in `crates/android/src` or the Kotlin sources
  (verified by grep); `#[allow(unsafe_code)]` appears only on `jni_fields.rs`
  with per-block `// SAFETY:` notes, consistent with the crate policy.
* **Config failures reach the launcher** as `failed` + message for the game
  dir, logging-init failure and any `ConfigError` (`lib.rs:92-152`) — all
  before `run_loop`, so none of them is clobbered (F2 affects only in-loop
  failures).
* **State/log write failures never panic**: `write_state` logs and swallows by
  design (`state.rs:118-131`), the log sink reports its own failure once to
  stderr (`log_file.rs:150-166`) and an unopenable log file is a typed startup
  error rather than a silent stderr-only fallback (`lib.rs:210-218`) — the D11
  story here is correct.
* **Silent-fallback scan:** F6 is the only D11-relevant fallback found on the
  JNI path. The remaining wildcard matches are over `#[non_exhaustive]`
  platform enums (`android_activity::{MainEvent, InputEvent}`) with documented
  intent, and the Kotlin reconcile's `else ->` treats an unknown future state
  as a clean exit *by comment* (`MainActivity.kt:229-230`) — a deliberate
  forward-compat choice, not a swallowed error.

---

## Verified by reading both sides (no device needed)

1. All six JNI static fields: name, type, declaring class, and the
   before-`super.onCreate` write order.
2. The full state-file contract: keys, vocabulary, null handling, atomicity,
   marker-vs-state mtime logic, and every Rust exit path's terminal write
   (which is where F2/F3/F4 live).
3. The log contract: format, encoding, line endings, rotation vs tail bounds.
4. Config ranges/defaults/names on both sides; the `fit`-only-on-Android
   default and the desktop path's unaffectedness; the no-knob claim for
   texture resolution and audio buffering.
5. Input event-kind parity against the winit backend and the compat layer's
   expectations; gesture state machine coherence and its reference citations
   (spot-checked against `MainScene.cpp:494-496`, `:820-829`, `:1097-1101`,
   `:1017-1146` — all present and consistent).
6. Absence of `#[allow(dead_code)]`, `todo!`, `unwrap`/`expect` on the JNI
   path, and target-gating (`#[cfg(target_os = "android")]` confined to
   `crates/android` and `crates/platform/platform/src/android.rs`, as allowed).

## Needs a device

* F1 reproduction (second launch in a warm process) — code path is
  deterministic from the `android-activity` docs; the device only confirms the
  process survives between launches.
* F5 frequency of `TerminateWindow → InitWindow` with AGDK `GameActivity`.
* F3 which OEM Vulkan drivers fail `Host::new`.
* Whether Android always delivers `ACTION_CANCEL` before `Pause` (phantom
  long-press window in `gesture_tick`).
* The whole documented "not verified on hardware" list in `docs/android.md`
  §8 still stands: motion-event ordering into the queue, synthesized clicks
  landing in a real KAG menu, `<filesDir>` writability, and the Logs screen
  reading real records.

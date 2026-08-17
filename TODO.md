# krkr-rs — Game Runtime TODO

## Current state (Aug 17, 2026)

**Working**: mount `.xp3` (23,572 + 174 entries) → run `startup.tjs` via the C++
TJS2 VM → `system/Status.tjs` + `Initialize.tjs` + `k2compat/*` + all
`system/*.tjs` load → `begin.tjs` creates `SceneManager` + `Logo` scene →
Bevy window renders the logo/title layers → input bridge dispatches mouse/key
to the game's `onMouseDown`/`onKeyDown` → `WaveSoundBuffer` audio natives
registered (BGM path wired, voices decode as silence).

**Remaining blockers**: the startup plugin chain is emulated by built-in Rust
natives. The gameplay gaps: the logo→title transition is **stuck at the
warning (ATTENTION) screen** — the voice-driven sequence advances only when
`onStatusChanged("stop")` fires, which is still being debugged. Below is the
complete investigation log.

---

## The white-screen investigation (Aug 17) — root causes found & fixed

The user-visible symptom was "a pure white block, no logo, no title". Four
independent bugs stacked on each other; each was found with native-level
instrumentation against the real game:

### 1. `OnceCall` / `OnceCallCancel` were missing (FIXED)
The game is a **Kirikiroid2 fork** (`@set(kirikiriz=1)`), not stock KiriKiri2.
`system/Title.tjs:158` calls `OnceCall(step01, 1000)` as a **bare global**
(also in EyeCatch.tjs, AttentionVoice, etc.). It's not in the reference C++
core, not in any game script — it's a Kirikiroid2-native. The Logo constructor
threw there → the logo keyframe chain never started → the scene was a static
white fill (LAYER_LOGO paints `fillRect(0,0,1280,720,0xffffffff)` at
`absolute = LAYER_LOGO (110000)`).

**Fix**: `register_timer` (tvp-visual/src/natives/timer.rs) now execs a script
that defines `OnceCall(fn, ms)` / `OnceCallCancel(fn)` as globals over the
`Timer` native class, with a function→timer registry (mirrors the game's own
`OnceTimer` script class).

### 2. `Timer.enabled = true` fired immediately instead of after the interval (FIXED)
Our native set `next_fire_ms = 0` on enable, but the reference
(`TimerImpl.cpp SetEnabled`) does `SetNextTick(now + interval)`. Added
`last_now_ms` to each TimerState; enable/interval changes reschedule from the
last polled clock. This made `OnceCall(fn, 100)` fire at once instead of after
100 ms.

### 3. Sound native context was thread-local (FIXED)
`NATIVE_CTX` in tvp-sound/src/natives.rs was `thread_local!`, but **Bevy's
parallel scheduler runs `Startup` and `Update` systems on different worker
threads** (we observed ThreadId 12/13/14). `register_sound` set the ctx on one
thread; `sound_poll` in `run_vm` ran on another → "sound natives are not
registered" → the game's `PlaySystemVoice` failed silently → the ATTENTION
sequence never advanced. Switched to a process-global `static Mutex`.

### 4. `WaveSoundBuffer(owner)` retained the wrong object (FIXED)
The reference (`SoundBufferBaseIntf.cpp Construct`) does
`ActionOwner = param[0]` — the **first constructor argument** is the action
owner. We retained `objthis` (the newly-created instance) instead. The game's
`AttentionVoice` does `new WaveSoundBuffer(this)` and implements
`action(ev)` (checks `ev.type == "onStatusChanged" && ev.status == "stop"`);
events went to the instance (native no-op `onStatusChanged`) and never reached
the AttentionVoice. Now the ctor retains argv[0], and `sound_poll` delivers
BOTH `action(%[type,status])` (dict, via a new `TjsValue::Retained` ABI path)
and `onStatusChanged(status)`.

### 5. Voice files are Ogg Opus; symphonia 0.5 has no Opus decoder (WORKED AROUND)
`voice/*.ogg` are **Ogg Opus** (`OpusHead`), while `bgm/*.ogg` are Vorbis.
symphonia 0.5.5 has no `symphonia-codec-opus` (added in 0.6+). `decode_audio`
now detects Opus and returns a ~60 ms silent buffer (the voices still drive
the script sequencing via `onStatusChanged("stop")`). Real Opus decoding needs
a symphonia 0.6 upgrade — see Stage 4.

### 6. Cross-thread deadlock in the sound natives (FIXED — run_vm lock)
`sound_poll` was observed running on **multiple Bevy worker threads**
(ThreadId 12/13/14) while timer callbacks ran on the main thread. Both take
the shared `STREAMS`/mixer locks; a ctor on thread A blocked on `STREAMS` held
by `sound_poll` on thread B → all threads futex-wait (verified via
`/proc/*/task/*/wchan` = `futex_do_wait`). **Fix**: `run_vm` now wraps the
entire VM step (async triggers + timers + continuous handlers + sound poll) in
a process-global `VM_RUN_LOCK` (`try_lock`, skip if another thread is already
driving the VM). The VM and all native state are now serialized even though
Bevy moves the system across threads.

---

## Stage 1 — Plugin registration matrix

| Plugin | Game use | krkr-rs status |
|---|---|---|
| `extrans.dll` | `Trans` class | ✅ native `Trans` registered (extrans.rs) |
| `csvParser.dll` | `new CSVParser()` (charData.csv) | ✅ real native in tvp-storages |
| `layerExDraw.dll` | layer effects (blur etc.) | ⚠️ logical-layer pixel ops; GPU blend modes pending |
| `fstat.dll` | `Storages.stat`/file metadata | ✅ stat/fstat metadata, copyFile/deleteFile |
| `windowEx.dll` | `System.desktop*`, monitor info | ✅ monitor context + `getDisplayMonitors` |
| `KAGParserEx.dll` | placeholder | ✅ KAGParser native |
| `getSample.dll` | debug only | ignored |
| `wuvorbis.dll` | `WaveSoundBuffer` (.ogg) | ✅ natives; Opus voice decode pending |
| `menu.dll` | `MenuItem`, `Window.menu` | ✅ logical tree + Window.menu fallback |
| `KAGParser.dll` | `ScController extends KAGParser` | ✅ native + script-subclass ctor |

## Stage 3 — Game-runtime milestones

1. **Logo → Title transition** — the chain now RUNS (OnceCall fires, fades
   progress, timers 7/8/9 fire, continuous handlers register/self-remove).
   **Stuck at the ATTENTION screen**: the voice sequence needs
   `onStatusChanged("stop")` from the WaveSoundBuffer owners. The owner/action
   delivery is fixed; next check is whether the AttentionVoice's `action(ev)`
   advances with the silent-Opus voices.
2. **Title screen input** — input bridge wired; verify click→skip-logo and
   NEW GAME / CONTINUE hit the SelectItems.
3. **`ScController` scenario loop** — `loadScenario → getNextTag → onTag`;
   needs `Layer.drawText` + fonts (tvp-text) + hit-testing for click-through.
4. **BGM/SE/voice** — rodio output wired (main-thread guard held in
   `VmRuntime`); real Opus voice decode pending (symphonia 0.6 upgrade).
5. **Save/load** — saveStruct eval OK; `savedata/` dir created at startup;
   full flow untested.

## Stage 4 — Known open issues

- **Symphonia 0.5 lacks Opus**: upgrade to symphonia 0.6 +
  `symphonia-adapter-libopus` so voices actually decode (currently silence).
  Requires adapting the decode API (0.5→0.6 broke some interfaces).
- **Blend modes**: `layer.type` (ltAdditive etc.) reaches the scene but Bevy
  still source-over blends — real GPU blend modes pending (render crate).
- **Hierarchy flattening**: parent/child order/opacity composed depth-first;
  verified correct in window_layer_order but the game's Logo uses flat layers.
- **`System.screenWidth/Height`** — logical size stays 1280×720; desktop
  origin/size supplied by Bevy.
- **Audio output**: cpal `Stream` is `!Send`/`!Sync` — the guard must be
  created AND dropped on the main thread (held in `VmRuntime`); no device →
  silent advance (never panics).

## How to verify

```bash
# release build
cargo build --release
# headless load (no window): must print "startup.tjs executed successfully"
./target/release/krkr-rs run "/mnt/DATA/Games/Others/test" --headless
# windowed run: logo → title transition, then click NEW GAME
./target/release/krkr-rs run "/mnt/DATA/Games/Others/test"
# unit + integration (full speed, parallel)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

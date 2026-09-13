# krkr-rs — Game Runtime TODO

## Current state (Sep 13, 2026)

**Working**: mount `.xp3` (23,572 + 174 entries) → run `startup.tjs` via the C++
TJS2 VM → `system/Status.tjs` + `Initialize.tjs` + `k2compat/*` + all
`system/*.tjs` load → `begin.tjs` creates `SceneManager` + `Logo` scene →
Bevy window renders the logo/title layers (the `AffineLayer`/`Sprite`
`onPaint`→`assignImages` composite now promotes the inner `_image` bitmap to
the visible parent) → **the full logo → ATTENTION → title transition
completes** (voice-driven, verified headlessly by `attention-harness`) → the
title scene constructs its `SelectItem`s → input bridge dispatches mouse/key
to the game's `onMouseDown`/`onKeyDown` with **engine-side layer hit-testing**
(sprite sheets + `0×0` affine containers) and an aspect-correct cursor→game
mapping (matches the camera's `AutoMin` scale, so hit areas follow the
rendered buttons at any window aspect) → `WaveSoundBuffer` audio natives with
**real Ogg Opus voice decode** → **NEW GAME starts the debut scenario
`scenario/01_01.ks`** (the `KAGParser`/`Scripts` VM contexts are
process-global; the real `GdiPlus` plugin is implemented in `tvp-visual`).
The scenario loop runs `onflag → scene → hide/blackout/cg/update → playse →
talk/ch → hitret` and stops at the first `hitret` click-wait. **Dialogue text
rasterizes** (`Layer.drawText` takes alpha from `opa`, not the color high
byte) and lands inside the message frame (`fillRect` no longer moves the
layer). `Layer.parent`/`window` return real `null`, and `Scripts.eval`/`exec`
return object/function results, so the game's `GetAbsolutePos`/Action paths
no longer throw. `Layer.drawPolygon/drawRectangle/drawLine/drawArc/drawBezier`
rasterize `GdiPlus.Appearance` brushes/pens. The window scales the 1280×720
scene on resize, and `System.exit` / window close quit gracefully. An
Android/WASM feasibility assessment lives in `docs/portability.md` and a
`GdiPlus` design spec in `docs/gdiplus.md`.

**Remaining blockers**: the startup plugin chain is emulated by built-in Rust
natives; `@update`/`@blackout` transition interpolation is a stub (tags run,
animation jumps); `GdiPlus` hatch/antialiasing are approximations; and
save/load is unported. Below is the investigation log; the logo→title
milestone is closed.

---

## Logo → title milestone closed (Sep 13)

The last blockers that kept the sequence frozen were found and fixed:

### A. Opus voice decode (FIXED)
Registered `symphonia-adapter-libopus` in a custom codec registry, so
`voice/*.ogg` (Ogg Opus) decode to real PCM with their true durations; the
silent ~60 ms fallback is now only a last resort. `AttentionVoice.action(ev)`
advances one entry per voice with the real (multi-second) timing.

### B. Reference-faithful sound event delivery (FIXED)
`WaveSoundBuffer` now retains **both** `objthis` (event target) and constructor
argument 0 (action owner), mirroring `SoundBufferBaseIntf.cpp`. `sound_poll`
posts `onStatusChanged` to the *instance*, so a script subclass override (the
game's `SoundBuffer.onStatusChanged`, which chains BGM and sets `_destroy`)
runs first and its `super.onStatusChanged(...)` reaches the native handler,
which forwards `%[type,status]` to `action(ev)`. Previously we called
`action` *and* `onStatusChanged` on the action owner, so the `SoundBuffer`
override never ran and every plain `WaveSoundBuffer(this)` logged a spurious
`Member "" does not exist`.

### C. Timer self-deadlock on a failing callback (FIXED)
`tvp-visual` `timer_on_timer` left its `TIMERS` guard alive across the
callback error branch, which re-locked the same non-reentrant `Mutex`:
whenever a script `onTimer` raised, the VM thread self-deadlocked. This hung
the title transition (a `finalize` error fired inside a timer). The guard is
now scoped and the error path re-locks safely.

### D. Missing native `finalize` on every native class (FIXED)
The reference's class macro always includes `TJS_DECL_EMPTY_FINALIZE_METHOD`,
so a script subclass can call `super.finalize()` (e.g. the game's
`AffineLayer.finalize`). The ABI now auto-registers a no-op `finalize` on
every instance class unless it provides its own.

### E. `Layer.hitType` / `Layer.cursor` (FIXED)
`SelectItemBase` sets `hitType = htMask` and `cursor = crDefault`; both now
exist as Layer properties (values stored in `LayerState`, hit-testing
semantics still pending).

### F. Object-valued `Layer.parent` + `Layer` member surface (FIXED)
The title scene builds a layer hierarchy with `.parent = <Layer object>`, so
`parent` is now a real object-aware property: the getter returns the parent
Layer's retained TJS object, the setter accepts a Layer object (its `id` is
read through a new `tjs2_prop_get` ABI) or an integer id, and the `Layer`
constructor resolves an object parent too. The same ABI generalizes
`setBitmap`/`setImage`/`copyFromBitmapToMainImage` to accept `Bitmap`
objects. Added `Layer.face` / `Layer.holdAlpha` and a batch of no-op stubs
(`setMainPixel`, `bringToBack`, `focusNext`, …), plus a
`WaveSoundBuffer.PhaseVocoder` stub class for the filtered title BGM.

### G. Embedded WGSL path (FIXED)
`LayerBlendMaterial` referenced `embedded://render/…`, but the crate's lib
name is `krkr_render`, so the embedded asset was never found (Bevy logged
`Path not found`). Corrected to `embedded://krkr_render/…`.

After F/G the title scene constructs end-to-end with **zero TJS
exceptions** and stays stable (`attention-harness` climbs through the logo,
runs the ATTENTION voices, changes to scene 2, and idles cleanly).

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

### 5. Voice files are Ogg Opus; no Opus decoder in the default registry (WORKED AROUND)
`voice/*.ogg` are **Ogg Opus** (`OpusHead`), while `bgm/*.ogg` are Vorbis.
Upgraded to **symphonia 0.6.1** (decode API migrated: `probe()`/`probe`,
`default_track(TrackType::Audio)`, `make_audio_decoder`, `copy_to_slice_interleaved`)
and **rodio 0.22** (`DeviceSinkBuilder::open_default_sink` → `MixerDeviceSink`).
The default codec registry still has no Opus decoder (it ships separately as
`symphonia-adapter-libopus`, which needs the C libopus). `decode_audio` detects
Opus and returns a ~60 ms silent buffer so the voices still drive the script
sequencing via `onStatusChanged("stop")`. Real Opus decoding is a follow-up —
see Stage 4.

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
| `layerExDraw.dll` | layer effects (blur etc.) | ⚠️ logical-layer pixel ops; GPU blend modes ✅ |
| `fstat.dll` | `Storages.stat`/file metadata | ✅ stat/fstat metadata, copyFile/deleteFile |
| `windowEx.dll` | `System.desktop*`, monitor info | ✅ monitor context + `getDisplayMonitors` |
| `KAGParserEx.dll` | placeholder | ✅ KAGParser native |
| `getSample.dll` | debug only | ignored |
| `wuvorbis.dll` | `WaveSoundBuffer` (.ogg) | ✅ natives + real Ogg Opus/Vorbis decode |
| `menu.dll` | `MenuItem`, `Window.menu` | ✅ logical tree + Window.menu fallback |
| `KAGParser.dll` | `ScController extends KAGParser` | ✅ native + script-subclass ctor |

## Stage 3 — Game-runtime milestones

1. **Logo → Title transition** — ✅ **CLOSED** (Sep 13). `OnceCall` chain
   runs, the ATTENTION voice sequence advances one entry per real Opus voice,
   and `step06` calls `game.changeScene(SCENE_TITLE)`. Verified headlessly by
   `attention-harness` (scene 0 → 2, logo closed) after fixing the Opus
   decode, the reference sound ownership, the timer self-deadlock, the
   native `finalize`, and `Layer.hitType`/`cursor`.
2. **Title screen input** — title scene constructs its `SelectItem`s (after
   the `hitType`/`cursor` fix). Next: wire hit-testing so click→skip-logo and
   NEW GAME / CONTINUE activate the items.
3. **`ScController` scenario loop** — `loadScenario → getNextTag → onTag`;
   needs `Layer.drawText` + fonts (tvp-text) + hit-testing for click-through.
4. **BGM/SE/voice** — rodio output wired; **real Opus voice decode done**
   (`symphonia-adapter-libopus`); Vorbis BGM decodes for real.
5. **Save/load** — saveStruct eval OK; `savedata/` dir created at startup;
   full flow untested.

## Stage 4 — Known open issues

- **Opus voice decode**: ✅ done — `symphonia-adapter-libopus` is registered
  in a custom codec registry; voices decode to real PCM (the ~60 ms silent
  buffer remains only as a probe/decode-failure fallback). BGM (Vorbis)
  already decodes for real.
- **Blend modes**: ✅ done — `render::blend` maps `layer.type` to real GPU
  blend states (source-over / add / reverse-subtract / replace) via a
  `LayerBlendMaterial` + `Material2d` pipeline variants.
- **Hierarchy flattening**: parent/child order/opacity composed depth-first;
  verified correct in window_layer_order but the game's Logo uses flat layers.
- **`System.screenWidth/Height`** — logical size stays 1280×720; desktop
  origin/size supplied by Bevy.
- **Audio output**: cpal `Stream` is `!Send`/`!Sync` — the guard must be
  created AND dropped on the main thread (held in `VmRuntime`); no device →
  silent advance (never panics).

## Stage 5 — Rendering architecture (long-term)

**Custom `wgpu` renderer integrated with the TJS engine** (roadmap; not
started). The current Bevy renderer works, but TVP's compositor is an
immediate-mode 2D layering model, not an ECS scene graph, and Bevy's
parallel scheduler + thread-affine resources (VM, cpal `Stream`) keep
forcing workarounds (`VM_RUN_LOCK`, main-thread audio guards, the
`DefaultPlugins`/`MinimalPlugins` dual path, `Material2d` specialization
for four fixed blend modes).

Plan (adopt incrementally, behind the existing `Scene` snapshot seam):

1. Keep Bevy while gameplay semantics are the bottleneck. First remove the
   *thread-affinity* pain with Bevy-native tools: make the VM and the audio
   sink `NonSend` resources (main-thread pinned) and drop `VM_RUN_LOCK`.
2. Formalize the seam: `sync.rs` already reads the shared `Scene` and emits
   draws — extract a backend boundary (`Scene` → draw list).
3. When the *rendering model* (transitions, affine layers, masks, text
   effects, movie) is what blocks progress, implement a standalone
   `wgpu` + `winit` backend behind that boundary: one thread owning VM +
   natives + mixer + queue, batched instanced quads, one pipeline per blend
   mode, deterministic frame timing.

Do **not** rewrite while the renderer is ahead of the game logic; the
motivation is the compositing model and thread determinism, not raw speed.

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

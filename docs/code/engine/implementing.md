# `engine` — implementing an engine that meets the contract

Source: `crates/engine/src/` · the obligations an engine implementation carries
· layer **L2**

This page states what an engine implementation owes the layer above it and
the layer below it. It is a **contract**, not a walkthrough: each obligation
names the rule it comes from, the way a replacement can tell whether it is
meeting it, and — where the current engine shows one way of meeting it — the
code that does.

The audience is a reader who wants to write a **different engine**, or a
**different backend behind the same engine**, and still be driven by the
compatibility layer (L3) or by their own application.

| topic | document |
|---|---|
| the current engine, module by module | [README.md](README.md) |
| the architecture this contract implements | [`arch/l2.md`](../../arch/l2.md) |
| the layers, the crate map and the frame in one page | [`arch/overall.md`](../../arch/overall.md) |
| the rules this contract restates | [`rules/arch_rules.md`](../../rules/arch_rules.md) · [`rules/api_rules.md`](../../rules/api_rules.md) |
| the L3 ↔ L2 protocol, item by item | [`code/compat/engine_api.md`](../compat/engine_api.md) |
| the public surface a replacement must offer | [`arch/l2-api.md`](../../arch/l2-api.md) |

---

## 1. Who this is for, and what "an engine" means here

### 1.1 The scope

An **engine** here is the part between a host application and a platform
backend. It owns:

* every evolving value the game has — the scene graph, the bitmaps, the
  storage namespace, the timer registry, the input state, the mixer, the
  text engine, the video overlay, the transition machines;
* the frame: the clock, the timer pump, the projection and the single draw
  call.

It performs actions on the stateless platform services below it — the file
system, the render pipeline, the media codecs — and keeps no process-global
state.

It sits between:

```
  L4  vm        the script host
  L3  compat    the compatibility surface: script classes, KAG, the event adapter
  L2  engine    ← an engine lives here: the state and the frame
  L1  platform  api · fs · media · psd · render · platform — stateless services
  app           the composition root: wiring only
```

### 1.2 What a replacement does not have to do

* **It does not have to be a KiriKiri-compatible engine.** Nothing in this
  contract names the external engine's classes, its member names, its
  numbering or its error strings.
* **It does not have to speak TJS.** The engine never sees a script value, a
  script object or a script exception.
* **It does not have to reproduce the external engine's behaviour.** That is
  the compatibility layer's business, expressed above the engine.

### 1.3 What a replacement does have to do

* own every evolving value as a field of an object;
* own the frame, and run it in the fixed order;
* own the mapping between the script-visible coordinate space and the device
  space;
* report every failure as a typed error value.

### 1.4 The two shapes a replacement takes

| shape | what changes | what stays |
|---|---|---|
| **a different engine** | the whole L2 crate: the scene graph, the subsystems, the frame | the obligations in §3, and the seam through which L3 reaches it |
| **a different backend** | the L1 services under the engine: the render pipeline, the media codecs, the platform host | the engine's own obligations, and the L1 rule that a service is stateless and takes the evolving state as an explicit `&mut` value |

---

## 2. The contract in one page

### 2.1 What a host depends on

| the host needs | from | to do what |
|---|---|---|
| an engine object | the engine | own it, and drive one frame per iteration of its loop |
| a frame entry point | `Engine::run` / `Engine::step` / `Engine::frame` | inject the elapsed time, and receive the frame's report |
| an upward hook | `GameFrame` | run the work that belongs above the engine, inside the frame |
| a surface resize | `Engine::resize` | tell the engine the framebuffer changed |
| the scene | `Engine::scene` / `Engine::scene_mut` | read and mutate the logical tree |
| the presentation | `Engine::translation`, `Engine::vm_size`, `Engine::take_window_resize` | map platform input device→VM, and apply a script-side window size to the OS window |
| a typed failure | `EngineError` | tell a hook failure, an overlay failure and a render failure apart |

### 2.2 What the compatibility layer depends on

The compatibility layer reaches the engine through exactly one file,
`crates/compat/src/engine_api.rs`, and it owns none of the engine's state.

| need | from |
|---|---|
| the logical tree and the pixel buffers | `Scene`, `LayerState`, `BitmapState`, `FontState`, `Rect` |
| the storage namespace | `Storage`, `Location`, `StorageReadError` |
| the timer registry | `TimerState`, `TimerId`, `TimerCallback` — armed by L3, pumped by L2 |
| the input state | `InputState` |
| the frame clock | `FrameClock` — the same clock the script's tick count reads |
| the mixer, the text engine, the video overlay, the transitions | the corresponding L2 types |
| the pixel primitives and the layer operations | `engine::raster`, `engine::layer_ops` |

The full inventory is in [`code/compat/engine_api.md`](../compat/engine_api.md).

### 2.3 The shape of the dependency

```
  host (app)
    ├── owns ──────────────▶ Engine
    ├── implements ─────────▶ GameFrame          (the one upward edge)
    ├── reads/writes ───────▶ Scene, TimerState, InputState
    └── calls ──────────────▶ L1 services         (api · fs · render)

  Engine (L2)
    ├── owns ──────────────▶ Scene · TimerState · InputState · Mixer ·
    │                        TextEngine · VideoOverlay · Trans · Storage
    ├── owns ──────────────▶ the frame clock, the timer pump, the projection
    ├── projects ──────────▶ RenderScene         (an L1-shaped draw tree)
    └── calls ──────────────▶ L1 services         (api · fs · render)
```

Three things follow, and they are the load-bearing ones:

* **The host owns the engine.** The engine is a value the host holds; it is
  not a global and not a singleton.
* **The engine owns the state.** Every evolving value is a field of the
  engine or of a value the engine owns.
* **The platform is below the engine.** The engine calls the stateless
  services and passes them the state they mutate; nothing calls upward
  except the one frame hook.

---

## 3. The obligations

Each obligation names the rule it comes from and the way a replacement can
tell whether it is meeting it.

### 3.1 State ownership

**Obligation.** Every evolving value is a field of an object. No
`static mut`, no `thread_local!`, no `LazyLock` / `OnceLock` /
`OnceCell`, no process-global registry, no hidden cache, no id-keyed table
outside the object that owns it. Two engines run in one process without
interacting.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §3.2;
[`rules/api_rules.md`](../../rules/api_rules.md) §3.1 and §7.2;
[`code/engine/README.md`](README.md) §6.

**How to tell.** Point at every field of the engine and of every subsystem it
owns, and find a mutable owner for each. Ask of each: *who owns this while
the frame runs?* A value with no answer is a global in disguise. Then build
two engines in one process and drive both.

**Check.** The `stateless` gate check covers the L1 crates only — the
middle layer is excluded from it, because the middle layer owns state. For
the engine, the obligation is met by review and by the shape of the type,
not by a gate check.

**Worked example.** `Engine` holds `scene`, `timer`, `input`, `clock`,
`textures`, `render_scene`, `surface`, `render_state`, `translation`
and the counters as its own fields (`crates/engine/src/engine.rs`).
`Scene` holds the windows, layers, bitmaps and fonts
(`crates/engine/src/scene.rs`); `TimerState` holds the registry
(`crates/engine/src/timer.rs`); `Mixer` holds the channels
(`crates/engine/src/sound.rs`); `InputState` holds the input
(`crates/engine/src/input.rs`).

### 3.2 The frame contract

**Obligation.** The engine owns the frame. Nothing above the engine owns a
delta, accumulates a clock or calls a subsystem pump. The frame runs a fixed
order, and it has exactly **one** upward edge.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §4;
[`arch/l2.md`](../../arch/l2.md) §9;
[`arch/overall.md`](../../arch/overall.md) §4.

**The upward edge** is a trait the engine declares and the layer above
implements:

```rust
pub trait GameFrame {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
    fn after_project(&mut self, _drawn_layers: &[u32]) -> Result<(), String> { Ok(()) }
}
```

* `before_render` runs the work that belongs above the engine: the script
  tick, the event dispatch, the staged subsystem pumps. Its failure ends the
  frame before anything is projected or drawn.
* `after_project` runs after the projection, with the ids the projection
  emitted. A hook that changes the scene there is followed by a second
  upload and projection, so the change is in the frame being drawn.

**The frame returns** a report: the frame index, the elapsed time, whether
the frame reached the display, the backend's counters, and the measured
costs of the upload, prune, projection and draw phases.

**How to tell.** Search the engine for a place where a delta is accumulated,
a clock is read, or a subsystem pump is called from outside the frame. Each
hit is a violation. Then count the traits the engine declares that the layer
above implements: there is one.

**Check.** Review. `krkr-gate layering` does not count the upward edges.

**Worked example.** `GameFrame` in `crates/engine/src/engine.rs`; the
composition root implements it in `crates/app/src/game.rs` as `VmHook`.

### 3.3 Time

**Obligation.** Time enters the engine at one place per subsystem, through
an injected value. The frame clock is the engine's own object. The timer
pump belongs to the engine. The mixer and the video overlay keep their own
clocks.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §4 and §4.1;
[`arch/l2.md`](../../arch/l2.md) §8 and §9;
[`code/engine/clock.md`](clock.md).

**The clock is injected.** The engine holds no wall-clock source. The caller
supplies the frame's elapsed time through a `FrameTimeSource`, and the
engine advances its own `FrameClock` with it. A test supplies a fixed value
and drives the same code path as the running game.

**The timer pump belongs to the engine.** The registry's public surface is
four operations — create an interval, set an interval, arm or disarm,
remove — and its pump is a free function the engine's frame calls:

```rust
pub fn deliver_due(registry: &mut TimerState, now_ms: u64) -> usize;
```

The compatibility layer arms timers and never advances them. The layer above
decides whether to arm at all; the engine decides when the fire happens.

**The mixer and the video keep their own clocks.** The mixer's clock is the
device-driven one: the audio device's callback consumes the samples and
moves the playback positions, so the mixer's time is the device's time and
not the frame's. The video overlay's clock advances by an injected delta and
is compared against the sound's clock, so the picture follows the sound.
Both are L2 state with their own time base; neither reads a wall clock.

**How to tell.** Search the engine for a call that reads the system time.
Each hit is a violation unless it measures how long a phase took — the
frame report's counters are measurements, not inputs.

**Check.** Review; the `stateless` gate check does not cover the middle
layer. The frame's determinism is pinned by the engine's own tests.

**Worked example.** `FrameClock`, `FrameTimeSource` and
`InjectedFrameTime` in `crates/engine/src/clock.rs`; `deliver_due` in
`crates/engine/src/timer.rs`; `Mixer::mix_advancing` in
`crates/engine/src/sound.rs`; `VideoOverlay::advance` in
`crates/engine/src/video.rs`.

### 3.4 The coordinate spaces

**Obligation.** Three pixel spaces exist, and exactly one layer names more
than one of them.

| space | unit | who speaks it |
|---|---|---|
| **VM pixel** | one pixel of the logical screen (the primary layer) | scripts, the compatibility layer, the engine |
| **window pixel** | one pixel of the OS window's client area | scripts, the compatibility layer |
| **device pixel** | one pixel of the framebuffer | the platform layer |

**The engine owns the mapping.** A uniform `min(device / vm)` scale,
centred, with the margin cleared: fit with letterbox, and only that. There
is no mode enum and no configuration key. A size pair that cannot be fitted
is a typed error, not a substituted scale.

The mapping is carried as one transform on the projected tree's root. The
projected nodes keep their VM-space coordinates; the platform layer composes
the root transform during traversal and never interprets what the
coordinates mean.

**What a host must map.** The platform's pointer and touch positions arrive
in device pixels. The host maps them back into VM space through the engine's
own mapping before the script tick, so the engine's input state and the
compatibility layer's hit-test only ever see VM coordinates. Events without a
position — keys, buttons, the wheel, text, focus — pass through unchanged.

**What a host must not do.** It must not write the framebuffer size back into
the scene's window: an OS resize changes the framebuffer only, and a script's
window-size change travels the other way, through the engine's window-resize
request. It must not interpret the root transform, and it must not hand a
device coordinate to the engine or to the compatibility layer.

**How to tell.** Search the engine for a device-space value crossing its
boundary, and search the compatibility layer for the mapping's own methods.
Both searches come back empty. Then resize the framebuffer and check that
no value the script observes moved.

**Check.** Review, and the `layering` gate check's vocabulary rules for the
engine crate.

**Worked example.** `PixelTranslation` in `crates/engine/src/translate.rs`;
`Engine::sync_presentation` and `Engine::reproject_render_scene` in
`crates/engine/src/engine.rs`; the input mapping in
`crates/app/src/game.rs`.

### 3.5 Errors

**Obligation.** Where the intended behaviour cannot be performed, the
operation returns a **typed error**. There are no degraded substitutes, no
arbitrary size caps, no guess-and-continue, and no wildcard arm that
swallows a newly-added variant. A malformed input is an error value, never
a panic.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §6;
[`rules/api_rules.md`](../../rules/api_rules.md) §5 and §6.

**One error enum per domain**, owned by the crate that declares the domain,
and `#[non_exhaustive]` so a new failure mode is additive. A new failure
mode is a new variant, handled at the call site.

**How to tell.** For each fallible operation: is there a `Result` with a
domain error? Does the call site handle every variant? Is there any
`unwrap_or`, `.ok()`, `if let Ok(..)` or `let _ = ..` on a result that
could carry a real failure? Is there a panic reachable from malformed
input?

**Check.** `krkr-gate error-handling` scans `crates/**` for swallowed
errors, defaulted coercions and dropped results. It does not judge whether an
error enum is the right one for its domain; that is review.

**The one cap that is not a cap.** The decode pool bounds the bytes it holds
in flight (256 MiB). That is a resource limit on the engine, not a limit on
what a script may ask for: a script that loads a larger image is served, not
refused. A replacement that bounds its own memory does the same - the bound
is on the engine's working set, never on the script's request.

**Worked example.** `EngineError`, `TranslateError`, `TimerError`,
`StorageReadError`, `MountError`, `StartupError`, `TextError`,
`SoundError`, `VideoError` and `TransError` — one per domain, each
declared by the module that owns it.

### 3.6 Determinism

**Obligation.** A test drives the frame with an injected clock. No wall
clock is read in the frame path, and no sleep appears in a frame test. The
same code path runs under a test and under the running game.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §4;
[`code/engine/clock.md`](clock.md) §4;
[`arch/l2.md`](../../arch/l2.md) §9.

**How to tell.** Drive the frame from a test with an explicit value at each
step, and check the clock, the timers, the hook's arguments and the reported
costs. The frame's report carries measured durations, and those are the only
values that differ between two runs.

**Check.** The engine's own test suites. `crates/engine/tests/engine.rs`
pins the clock behaviour and the hook's arguments;
`crates/engine/tests/clock.rs` pins the clock itself.

**Worked example.** `Engine::frame(now_ms, hooks)` sets the clock to an
injected value, and `Engine::run(clock, hooks)` takes the elapsed time from
the caller's source; both reach `Engine::run_frame`.

### 3.7 Genericity

**Obligation.** The engine speaks no external-engine and no script
vocabulary. No `LT_*`, no `TVP_*`, no virtual-key codes, no `.sli`
fields, no `0..100000` volume scale, no KAG tags, no storage-name grammar.

**The compatibility layer owns** the script-visible names, the class and
member names, the enumeration numbering, the error strings, the storage
naming and the scenario semantics. A blend is a generic `BlendMode`; a gain
is normalized; a key code is an opaque integer.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §2.1 and §2.2;
[`rules/api_rules.md`](../../rules/api_rules.md) §3.3.

**How to tell.** Read the engine's vocabulary: it names concepts, not script
members. A member name that only means something to a script belongs one
layer up.

**Check.** `krkr-gate layering` rejects that vocabulary in the engine crate
(`l2-vocabulary`) and rejects a storage-name-shaped string literal
(`l2-storage-name`).

**Worked example.** `engine::scene::LayerState` holds a `blend` of the
platform type `BlendMode`, a normalized `opacity` and a generic `Rect`;
the script's `Layer.type` numbering is the compatibility layer's.

### 3.8 The layer boundary

**Obligation.** The engine depends on the platform **type** crates and on
nothing above it and on no platform backend.

| may depend on | may not depend on |
|---|---|
| `api` (DTOs and error enums), `fs` (file system and archive parse), `render` (render pipeline and font system) | the compatibility layer, the VM host, the vendored VM, the media crate, the platform crate, a windowing or graphics backend, an audio backend, `std::fs` |

The engine may name the L1 crates' types in its own public API — the draw
tree, the render state, the font metrics — because those types are the
boundary's shared vocabulary.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §2 and §2.1;
[`rules/api_rules.md`](../../rules/api_rules.md) §3.4;
[`code/engine/README.md`](README.md) §1.

**How to tell.** Read the engine's dependency list. Every entry is a type
crate or a pure library. Then search the engine for a window, a surface
handle, a file-system call or a codec call: each hit is a layer breach.

**Check.** `krkr-gate layering` and `krkr-gate android-target` — the
latter resolves the dependency graph per target and fails when a
desktop-only backend feature leaks into the Android target.

**Worked example.** `crates/engine/src/lib.rs` declares the module set and
the curated root re-exports; the crate depends on `api`, `fs`, `render`,
`cosmic-text`, `thiserror`, `log` and `ahash`.

### 3.9 The surface

**Obligation.** The compatibility layer reaches the engine through exactly
one file: `crates/compat/src/engine_api.rs`. No other file under
`crates/compat/src` names `engine::`. The seam is a re-export, not a
wrapper: a protocol type *is* the engine type.

**What may cross the seam.** Engine state, and actions on that state. Every
value crossing is a VM pixel. Every failure crossing is a typed error owned
by the module that declares it.

**What must not cross it.** The L1 services; the renderer, the draw tree and
the projection; the pixel translation itself; the engine object; the platform
backends; the game loading and the startup discovery; the VM.

**Adding an item.** Four steps, and the fourth is the record:

1. add or reuse the item in the engine, under an L2-generic name;
2. re-export it in the correct group of the seam;
3. use it from the compatibility layer through `crate::engine_api::…`;
4. record it in [`rules/api_rules.md`](../../rules/api_rules.md) §4 and in
   [`code/compat/engine_api.md`](../compat/engine_api.md).

**Removing an item.** Delete it from the engine, the seam, the protocol
document and every call site, in the same change.

**Rule.** [`rules/arch_rules.md`](../../rules/arch_rules.md) §11;
[`rules/api_rules.md`](../../rules/api_rules.md) §4.

**How to tell.** Search `crates/compat/src` for `engine::`. Every hit is
in the seam file.

**Check.** `krkr-gate layering` enforces this as the `l3-engine-api` rule.

**Worked example.** `crates/compat/src/engine_api.rs`, grouped by engine
module: the scene, the storage namespace, the input state, the timer
registry, the frame clock, the text engine, the mixer, the video overlay,
the transitions, the shader effect draw, the raster primitives and the layer
operations.

---

## 4. The frame, step by step

This is the sequence a replacement implements against. The current engine's
`Engine::run_frame` in `crates/engine/src/engine.rs` runs exactly this;
the clock is already set when it starts.

| # | step | code |
|---|---|---|
| 1 | the caller supplies the elapsed time, or the absolute clock value | `Engine::run` → `FrameClock::advance`; `Engine::step` → `advance`; `Engine::frame` → `FrameClock::set` |
| 2 | record the frame's clock | `Engine::run_frame` sets `last_now_ms` |
| 3 | fit the presentation to the current VM and device sizes | `Engine::sync_presentation` → `PixelTranslation::fit` |
| 4 | deliver the due timers | `timer::deliver_due(&mut self.timer, now_ms)` |
| 5 | publish the storage reads the previous frame scheduled | the storage pool's `drain` |
| 6 | **run the upward hook** | `GameFrame::before_render(now_ms, dt_ms)` |
| 7 | wait for the pending image decodes — the image barrier | `Scene::resolve_pending_images` |
| 8 | fit the presentation again — the hook may have resized a window's primary layer | `Engine::sync_presentation` |
| 9 | if the sizes cannot be fitted: report the frame as not presented and draw nothing | the `translation.is_none()` branch of `Engine::run_frame` |
| 10 | refresh the render-only overlay | `Engine::refresh_overlay` → `EngineOverlay::refresh` |
| 11 | upload the dirty bitmap regions, prune the textures whose bitmap is gone | `Engine::sync_dirty_bitmaps`, `Engine::prune_stale_textures` |
| 12 | project the scene when its structure revision changed | `Engine::reproject_render_scene` → `projection::project_scene_report` |
| 13 | put the Fit presentation on the render-scene root | `PixelTranslation::root_transform` |
| 14 | **run the upward hook again**, with the projected layer ids | `GameFrame::after_project(&self.drawn_layers)` |
| 15 | fit the presentation again | `Engine::sync_presentation` |
| 16 | upload and prune again — the hook may have rewritten a bitmap without changing the tree | `Engine::sync_dirty_bitmaps`, `Engine::prune_stale_textures` |
| 17 | project again if the hook changed the scene's structure | `Engine::reproject_render_scene` |
| 18 | draw the scene in one call | `render::render` |
| 19 | recover a lost surface; record whether the display accepted the frame | `render::recover_surface`, the presentation-stall bookkeeping |
| 20 | bump the frame counter and return the report | `FrameReport` |

Four properties of this order matter to a replacement:

* **The hook runs between the timer pump and the projection**, so the scene
  the layer above left behind is the one the frame presents.
* **A change made through `Engine::scene_mut` is a change the engine does
  not see.** The handle's writes do not mark the tree as structurally
  changed, so the projection is not redrawn until something else marks it.
  A host that mutates the tree that way marks it too.
* **The projection is skipped when nothing structural changed.** A bitmap
  content write re-uploads the texture without re-projecting the tree; a
  bitmap resize is structural, because the projection reads its dimensions.
* **The tree is projected in VM pixels** and the root carries the
  presentation transform. A changed presentation invalidates the projection.
* **A frame the display did not accept is reported as such.** The report
  carries whether the frame reached the display, and the engine keeps the
  stall's accounting across frames; a replacement reports a skipped frame in
  the same field rather than dropping it silently.
* **A frame with nothing to show does not draw.** The window keeps the
  previous frame's contents, and the change waits for a frame that has
  something to show.

---

## 5. What a replacement may change freely, and what it may not

### 5.1 The extension points

| extension point | how a replacement reaches it | what it may change |
|---|---|---|
| **the render backend** | `EngineParts::pipeline` | the whole draw path: the pipeline, the surface, the render state, the shaders, the effect pipelines. A backend that cannot run effect pipelines gets none, and the transitions that need one fall back to the CPU blend |
| **the image decoder** | `Engine::install_decoder` takes an `Arc<dyn ImageDecoder>` | the codec, the worker pool's configuration and its byte limit. A job the pool refuses is decoded on the calling thread instead |
| **the storage reader** | `Engine::install_storage_reader` takes an `Arc<dyn StorageReader>` | the backing of the reads: a live file, an in-memory image, another source. The resolution order and the namespace stay the engine's |
| **the storage namespace** | `Storage` | the mounts, the precedence, the auto paths, the resolution cache — all inside the engine, and all free to change as long as the observable order holds |
| **the mixer** | `Mixer` and the `AudioStream` seam | the mixing, the resampling, the loop model, the decode-ahead stream. The normalized gain and the clock discipline stay |
| **the text engine** | `TextEngine::add_font` takes bytes; `load_system_font` resolves a family | the shaping, the layout, the coverage rasterization, the glyph atlas, the caches. The metrics stay in VM units |
| **the video overlay** | `VideoOverlay` | the playback machine, the looping, the period events. The host supplies the decoded frames |
| **the shader programs** | `Engine::load_shader`, `Engine::effect_pipeline` | the programs, the pipelines, the blend slots |

### 5.2 The fixed points

| fixed point | why it cannot move |
|---|---|
| **the coordinate contract** | the script-visible values are VM pixels, the mapping is fit with letterbox, and the platform layer composes the root transform without interpreting it. A different mapping changes what a script observes |
| **the error discipline** | a typed error per domain, no silent fallback, no arbitrary cap. A replacement that guesses is a replacement the compatibility layer cannot report through |
| **the seam** | `crates/compat/src/engine_api.rs` is the only path from the compatibility layer to the engine. A second path is a second protocol |
| **the frame's order and its single upward hook** | the layer above's work runs inside the frame, between the timer pump and the projection |
| **state ownership** | two engines in one process, with no global between them |
| **the ownership direction** | the host owns the engine, the engine owns the state, the platform is below |

---

## 6. The obligations a replacement inherits from the tests

A replacement is conforming when it passes the suites below. The suites
belong to the layers, not to the engine: the engine's own suites prove the
engine's obligations, and the compatibility layer's suites prove that the
layer above can drive the replacement through the seam.

### 6.1 The engine's own suites — `crates/engine/tests/`

| suite | what it proves |
|---|---|
| `engine.rs` | the frame: the clock, the projection cadence, the upload retry, the presentation faults, the window-resize handoff, the hook's arguments |
| `clock.rs` | the frame clock: the fractional millisecond, the exact set, the saturation |
| `timer.rs` | the timer registry: the delivery order, the re-arm, the re-entrancy, the typed errors |
| `translate.rs` | the VM ↔ device mapping: the fit, the letterbox, the round trip, the pixel region |
| `projection.rs` | the L2 → L1 seam: the field copy, the composition, the clip, the order |
| `scene.rs`, `bitmap.rs`, `oop_semantics.rs` | the logical tree, the bitmaps, the focus, the ordering |
| `input.rs` | the input state and the frame protocol around it |
| `storage.rs`, `storage_async.rs`, `startup.rs` | the namespace, its resolution order, the startup discovery |
| `sound.rs`, `video.rs` | the mixer and the overlay machine |
| `trans.rs`, `trans_blend.rs`, `trans_effect.rs`, `trans_effect_gpu.rs`, `extrans_mosaic.rs`, `extrans_ripple.rs`, `extrans_rotate.rs`, `extrans_turn.rs`, `extrans_wave.rs` | the transition machine, its composition kinds and the effect draws |
| `text.rs`, `text_render.rs` | the shaping, the coverage, the atlas, the projection |
| `layer_ops.rs`, `raster.rs` | the pixel operations |
| `imagedecode.rs` | the decode pool: the parallelism, the byte limit, the pending image, the frame barrier |
| `pending_safety.rs` | the pending bitmap's pixel access and the province plane |
| `dirty.rs` | the scene's change tracking and the frame cadence it drives |
| `shader_library.rs`, `shader_assets.rs` | the shader-program registry and the shipped programs |

### 6.2 The compatibility layer's suites — `crates/compat/tests/`

A replacement that the compatibility layer drives must pass these. They
reach the engine only through the seam, so they are the real test of the
seam's sufficiency.

| suite | what it proves |
|---|---|
| `runtime_surface.rs` | the script-visible dispatch surface. The dump is compared with the committed baseline `docs/surface/runtime_surface.json`; a replacement that changes the script surface fails it |
| `layer.rs`, `layer_behaviour.rs`, `layer_geometry.rs`, `layer_face.rs`, `layer_hit_test.rs`, `layer_ex.rs`, `layer_ex_behaviour.rs`, `layer_ex_image.rs`, `layer_ex_pending.rs`, `window.rs`, `window_ex.rs`, `window_members.rs`, `window_size.rs`, `window_update.rs`, `visual.rs`, `dispatch_visual.rs` | the scene graph through the seam: the tree, the geometry, the focus, the hit-test, the window |
| `bitmap.rs`, `image_function.rs`, `pending_image.rs`, `pixel_semantics.rs` | the bitmaps, the image functions, the pending-image path, the pixel semantics |
| `storages.rs`, `storages_io.rs`, `storage_media_prefix.rs` | the storage namespace through the seam |
| `timer_behavior.rs` | the timers: the compatibility layer arms them and the engine fires them |
| `input.rs`, `event.rs`, `event_dispatch.rs`, `event_batch.rs` | the input state and the event adapter |
| `sound.rs`, `dispatch_sound.rs`, `audio_render.rs` | the mixer through the seam |
| `video.rs` | the video overlay through the seam |
| `text.rs`, `font.rs`, `text_render.rs` | the text engine through the seam |
| `transitions.rs` | the transition machine through the seam |
| `focus_behaviour.rs` | the focus domains and the keyboard traversal |
| `kag.rs`, `dispatch_kg.rs` | the scenario layer, which reaches the engine only through the scene |

### 6.3 The composition root's suites — `crates/app/tests/`

| suite | what it proves |
|---|---|
| `host.rs` | the host owns the engine and drives one frame per step |
| `frame_timing.rs` | the frame's measured costs and the slow-frame attribution |
| `boot_game.rs` | the whole stack, from the archive mount to the first frame |

### 6.4 The gates

`krkr-gate gate` runs the static checks that protect the obligations above,
and `krkr-gate test` runs the test suites. The checks that bear directly on
this contract are:

| check | the obligation it protects |
|---|---|
| `stateless` | the L1 half of the state-ownership obligation (§3.1) |
| `layering` | the vocabulary (§3.7), the layer boundary (§3.8), the seam (§3.9), the unsafe placement |
| `error-handling` | the error discipline (§3.5) |
| `house-style` | the shape of the public surface and the comments |
| `surface-lock` | the script surface (§6.2) |
| `android-target` | the per-target dependency graph (§3.8) |

---

## 7. A checklist

Before claiming that a replacement is contract-conforming:

**State**

- [ ] Every evolving value is a field of an object; no global, no static, no
      `thread_local!`, no process-wide registry.
- [ ] Two engines run in one process without interacting.

**Frame**

- [ ] The engine owns the frame; nothing above it owns a delta or a clock.
- [ ] There is exactly one upward edge, and the layer above implements it.
- [ ] The frame runs the order in §4, and the hook runs between the timer
      pump and the projection.
- [ ] The frame returns a report that says whether the frame reached the
      display.

**Time**

- [ ] No wall clock is read in the frame path.
- [ ] The frame clock is the engine's own object and is the same clock the
      script's tick count reads.
- [ ] The timer pump belongs to the engine; the layer above arms timers and
      never advances them.
- [ ] The mixer and the video overlay keep their own clocks, and neither
      reads a wall clock.

**Coordinates**

- [ ] Every value that crosses the engine's boundary is a VM pixel.
- [ ] The mapping is fit with letterbox, and an unfittable size pair is a
      typed error.
- [ ] The host maps platform input device→VM through the engine's mapping,
      and writes no framebuffer size back into the scene.
- [ ] The platform layer receives a VM-space tree with the root transform and
      interprets nothing.

**Errors**

- [ ] One typed error enum per domain, owned by the module that declares the
      domain.
- [ ] Every fallible operation returns a `Result`; every call site handles
      every variant.
- [ ] No silent fallback, no arbitrary cap, no guess-and-continue.
- [ ] No panic is reachable from malformed input.

**Generality and boundaries**

- [ ] The engine names no script-visible vocabulary.
- [ ] The engine depends on the platform type crates and on no backend, and
      on no windowing, audio or codec code.

**Surface**

- [ ] The compatibility layer reaches the engine through the seam file only.
- [ ] Every seam item is engine state or an action on it; every value
      crossing is a VM pixel; every failure crossing is a typed error.
- [ ] Each new seam item is recorded in the protocol document and in the
      rules.

**Verification**

- [ ] `crates/engine/tests/` passes.
- [ ] `crates/compat/tests/` passes.
- [ ] `crates/app/tests/` passes.
- [ ] `krkr-gate gate` and `krkr-gate test` are green.

---

## 8. The current engine as the worked example

One row per obligation, and the code that meets it.

| obligation | where the current engine meets it |
|---|---|
| §3.1 state ownership | `Engine`'s fields in `crates/engine/src/engine.rs`; `Scene` in `crates/engine/src/scene.rs`; `TimerState` in `crates/engine/src/timer.rs`; `Mixer` in `crates/engine/src/sound.rs`; `InputState` in `crates/engine/src/input.rs` |
| §3.2 the frame contract | `GameFrame` and `Engine::run_frame` in `crates/engine/src/engine.rs`; the hook's implementation in `crates/app/src/game.rs` |
| §3.3 time | `FrameClock`, `FrameTimeSource`, `InjectedFrameTime` in `crates/engine/src/clock.rs`; `deliver_due` in `crates/engine/src/timer.rs`; `Mixer::mix_advancing` in `crates/engine/src/sound.rs`; `VideoOverlay::advance` in `crates/engine/src/video.rs` |
| §3.4 the coordinate spaces | `PixelTranslation` in `crates/engine/src/translate.rs`; `Engine::sync_presentation` and `Engine::reproject_render_scene` in `crates/engine/src/engine.rs`; the input mapping in `crates/app/src/game.rs` |
| §3.5 errors | `EngineError` in `crates/engine/src/engine.rs`; `TranslateError` in `crates/engine/src/translate.rs`; `TimerError` in `crates/engine/src/timer.rs`; `StorageReadError`, `MountError` and `StoragePoolError` in `crates/engine/src/storage.rs`; `StartupError` in `crates/engine/src/startup.rs`; `TextError` in `crates/engine/src/text.rs`; `SoundError` in `crates/engine/src/sound.rs`; `VideoError` in `crates/engine/src/video.rs`; `TransError` in `crates/engine/src/trans/mod.rs` |
| §3.6 determinism | `Engine::frame` and `Engine::run` in `crates/engine/src/engine.rs`; `crates/engine/tests/engine.rs`; `crates/engine/tests/clock.rs` |
| §3.7 genericity | `crates/engine/src/scene.rs` and `crates/engine/src/projection.rs`; the vocabulary rules in `krkr-gate layering` |
| §3.8 the layer boundary | `crates/engine/src/lib.rs` — the module set and the curated root re-exports |
| §3.9 the surface | `crates/compat/src/engine_api.rs`; the protocol in [`code/compat/engine_api.md`](../compat/engine_api.md) |
| §4 the frame | `Engine::run_frame` in `crates/engine/src/engine.rs`; the frame's code map in [`engine.md`](engine.md) |
| §5 the extension points | `Engine::install_decoder`, `Engine::install_storage_reader`, `Engine::load_shader`, `Engine::effect_pipeline` in `crates/engine/src/engine.rs`; `Storage` in `crates/engine/src/storage.rs`; `Mixer` in `crates/engine/src/sound.rs`; `TextEngine` in `crates/engine/src/text.rs` |
| §6 the tests | `crates/engine/tests/`, `crates/compat/tests/`, `crates/app/tests/` |

---

## 9. Where to read next

| topic | document |
|---|---|
| the engine's modules, one page each | [README.md](README.md) |
| the frame's code map | [engine.md](engine.md) |
| the frame clock | [clock.md](clock.md) |
| the timer registry | [timer.md](timer.md) |
| the coordinate mapping | [translate.md](translate.md) |
| the projection seam | [projection.md](projection.md) |
| the logical tree | [scene.md](scene.md) |
| the L3 ↔ L2 protocol | [`code/compat/engine_api.md`](../compat/engine_api.md) |
| the rules this contract restates | [`rules/arch_rules.md`](../../rules/arch_rules.md) · [`rules/api_rules.md`](../../rules/api_rules.md) |

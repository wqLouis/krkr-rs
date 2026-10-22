# `engine::engine` — the frame-owning object

Source: `crates/engine/src/engine.rs`

`Engine` is the object the composition root drives. It holds the logical
[`Scene`](scene.md), the projected L1 `RenderScene`, the surface/render-state
pair, the bitmap→texture table, the [`TimerState`](timer.md), the
[`InputState`](input.md) and the VM↔device
[`PixelTranslation`](translate.md) — and it owns the **frame**: the clock, the
timer pump, the projection and the single draw call.

**Responsibility.** Own every L2 state value the frame touches, so two
engines can run in one process; run the frame in the fixed order the
architecture fixes; keep the projected tree in step with the logical tree and
the texture table in step with the bitmaps; own the surface size and the
presentation mapping, and report a script-side window resize to the composition
root.

The engine does **not** own a `dt`: the frame clock is injected
(`frame(now_ms, …)`) or accumulated (`step(dt_ms, …)`). Nothing above L2 owns
the delta.

The frame contract (who owns the clock, what the upward hook is, how the
presentation is fitted) is stated once in
[`arch/l2.md`](../../arch/l2.md) §9 and
[`rules/arch_rules.md`](../../rules/arch_rules.md) §4. This page is the code
map for the module.

---

## 1. State it owns

| field | meaning |
|---|---|
| `pipeline: RenderPipeline` | the stateless L1 render service |
| `scene: Scene` | the logical layer tree |
| `render_scene: RenderScene` | the projected draw tree (an L1-shaped value) |
| `render_scene_revision: Option<u64>` | the `Scene::structure_revision` the projection was built from; `None` forces a rebuild |
| `drawn_layers: Vec<u32>` | the layer ids the latest projection emitted |
| `surface: Surface`, `render_state: RenderState` | the L1 surface and its GPU/staging bookkeeping |
| `textures: Map<u32, Texture>` | bitmap id → uploaded texture |
| `timer: TimerState` | the timer registry the frame advances |
| `input: InputState` | the script-visible input state |
| `surface_size: (u32, u32)` | the current framebuffer size in device pixels |
| `clock_ms`, `now_ms`, `last_now_ms`, `frame_index` | the frame clock and counters |
| `headless: bool` | whether the engine runs with no window |
| `overlay: Option<Box<dyn EngineOverlay>>` | the render-only debug overlay |
| `translation: Option<PixelTranslation>` | the current VM↔device mapping; `None` when the sizes cannot be fitted |
| `last_window_client_size`, `pending_window_resize` | the window client size the engine observed, and the resize the composition root still has to apply |
| `presentation_skipped_logged` | whether the unfittable-size warning was already logged |
| `stall: PresentationStall` | the run of frames the display did not accept: its frame count, start clock, distinct causes and next age report |

---

## 2. Public API

### Construction

```rust
pub struct EngineParts {
    pub pipeline: RenderPipeline,
    pub surface: Surface,
    pub render_state: RenderState,
    pub surface_size: (u32, u32),
    pub headless: bool,
    pub overlay: Option<Box<dyn EngineOverlay>>,
}

impl Engine {
    pub fn new(parts: EngineParts) -> Result<Self, EngineError>;
}
```

`Engine::new` clamps the surface size to at least `1x1`, performs the one
surface action the engine does before its first frame, adopts the extent the
backend actually took (a compositor or DPI clamp can differ from the request)
and fits the presentation.

### The frame

```rust
impl Engine {
    pub fn step(&mut self, dt_ms: f32, hooks: &mut dyn GameFrame) -> Result<FrameReport, EngineError>;
    pub fn frame(&mut self, now_ms: u64, hooks: &mut dyn GameFrame) -> Result<FrameReport, EngineError>;
    pub fn resize(&mut self, size: (u32, u32)) -> Result<SurfaceEvent, EngineError>;
}
```

* `frame(now_ms, …)` **sets** the clock exactly, discarding any fractional
  remainder left by an earlier `step`; the delta handed to the hook is the
  distance from the previous frame's clock (zero on the first frame), so a
  test can jump between frames deterministically.
* `step(dt_ms, …)` **adds** the full float delta to the engine's own clock; a
  negative delta counts as zero. The sub-millisecond part is retained across
  frames, so sixty `step(16.6)` calls advance the clock by 996 ms rather than
  960. The timer, hook and overlay receive the accumulated clock's whole
  millisecond `now_ms`.
* `resize(size, …)` rebinds the surface to a new framebuffer size. The
  logical screen is untouched: the scene keeps its window client sizes and
  primary-layer rects, and the presentation refits the screen into the new
  framebuffer, so an OS resize moves no value a script observes.

### The upward edges

```rust
pub trait GameFrame {
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
    fn after_project(&mut self, drawn_layers: &[u32]) -> Result<(), String> { Ok(()) }
}

pub trait EngineOverlay {
    fn refresh(&mut self, pipeline: &RenderPipeline, state: &mut RenderState,
               now_ms: u64, objects: ObjectCounts) -> Result<(), String>;
    fn node(&self) -> RenderNode;
}
```

`GameFrame` is the **single upward edge** of the frame: the layer above runs
its VM tick and its event dispatch here. `after_project` sees the projection's
actual emit report; a hook that changes the scene there is followed by a
second upload and projection, so its change is in the frame being drawn.

`EngineOverlay` supplies the render-only debug overlay. It is not part of the
scene and owns no bitmap a script could reach, so it cannot change the layer
tree.

### Inspection

`pipeline()`, `backend()`, `scene()`, `scene_mut()`, `render_scene()`,
`drawn_layers()`, `textures()`, `input()`, `input_mut()`, `timer()`,
`timer_mut()`, `is_headless()`, `surface_size()`, `vm_size()`,
`object_counts()`, `translation()`, `take_window_resize()`, `frame_index()`,
`elapsed_ms()`, `last_frame()`, `last_frame_timings()`,
`debug_overlay_enabled()`.

`vm_size()` is the **script-visible screen size**: the first window's
primary-layer rect, the window's client size while it has no primary layer,
or the framebuffer size when the scene has no window.

`take_window_resize()` hands the composition root the script-side window size
it still has to apply to the OS window, and clears it, so a size that does not
change again is not requested twice.

### The report

```rust
pub struct FrameReport {
    pub frame_index: u64,
    pub elapsed_ms: f64,
    pub presented: bool,
    pub stats: Option<FrameStats>,
    pub sync_ms: f64,
    pub upload_ms: f64,
    pub prune_ms: f64,
    pub project_ms: f64,
    pub render_ms: f64,
}
```

`stats` is the null backend's counters (`None` on a GPU backend). `presented ==
false` means the scene was updated but the backend kept the previous surface
contents; the engine counts the frame into the current `PresentationStall` —
the reference presents on every vsync wake, and a wake that produced no frame
is a skipped, reported frame
(`reference/cpp/core/visual/impl/VSyncTimingThread.cpp:114,141-142`). The
engine does not retry a presentation inside the same frame: the backend already
rebuilds its swapchain at the start of the next frame when it needs to, and a
skip has nothing to retry.

A stall is a **run**, not a per-frame fault. The first skipped frame opens it;
each distinct cause warns once (`engine: the display did not release a
swapchain image within the presentation budget; this frame was skipped` and
its siblings); at one second and then every ten seconds it reports its age and
frame count, so a display that stops accepting frames is never silent; and the
next presented frame closes it with
`engine: presentation resumed after {frames} skipped frame(s) in {seconds} s:
{causes}`. The causes are the distinct reasons the frames were skipped — an
`AcquireTimeout` from the display, a lost or out-of-date surface, a backend
that presented nothing, or a game surface that cannot be fitted into the
framebuffer — so a resume that follows an occlusion, a resize storm and an
unfittable size names all three instead of only the last one.

---

## 3. The frame, in order

`Engine::advance(now_ms, clock_ms, dt_ms, hooks)` runs exactly this sequence:

1. set the clock (`clock_ms`, `now_ms`, `last_now_ms`);
2. `sync_presentation()` — refit the VM↔device mapping, observe the window
   client size;
3. `timer.advance(now_ms)` — fire the due timers;
4. `hooks.before_render(now_ms, dt_ms)`;
5. `sync_presentation()` again — the hook may have resized a window's primary
   layer;
6. **if the sizes cannot be fitted** → count the frame as not presented, skip
   everything that draws and return a report with no statistics;
7. `refresh_overlay(now_ms)`;
8. upload the dirty bitmap regions, prune the textures whose bitmap is gone;
9. project the scene when `Scene::structure_revision` changed (or the
   presentation/texture table invalidated the projection);
10. `hooks.after_project(&drawn_layers)`;
11. `sync_presentation()` again;
12. upload the dirty bitmap regions again (a CPU-blended transition frame is a bitmap
    the hook rewrote without changing the tree) and prune again;
13. project again if the hook changed the scene's structure;
14. one `render` call;
15. recover a lost surface, then record the frame in the presentation stall —
    a presented frame closes the run and reports its span and causes, a
    skipped one records its cause (warned once per cause) and the stall's age
    when it is due, and the next frame's render call rebuilds a stale
    swapchain;
16. bump the frame counter and return the `FrameReport`.

The projection is rebuilt **in place**: the node list is cleared (retaining its
capacity) and refilled, so only the projected nodes' own allocations are
touched.

---

## 4. Invariants

* **The engine owns the clock.** Nothing above L2 accumulates a `dt` or calls
  a subsystem pump itself. The clock is one `f64` millisecond accumulator:
  `step` adds to it and saturates at the integer ceiling, `frame` replaces it
  with the injected whole value, and the timer and upward edge receive its
  whole-millisecond part.
* **The projected tree is in VM pixels.** The render-scene root carries the
  Fit presentation transform; the overlay is attached with the inverse root
  transform because its node is authored in device coordinates.
* **A changed presentation invalidates the projection**, because the tree is
  built in VM pixels.
* **A bitmap whose texture was missing at the last projection is projected
  again**, because the node it was skipped for now has a texture.
* **A failed upload leaves the bitmaps dirty.** The batch did not reach the
  backend, so clearing the flag would present a frame that silently omits the
  texture; the next frame retries.
* **A bitmap with a zero dimension or an inconsistent pixel length is skipped
  with a warning**, so one bad bitmap cannot abort the frame.
* **The framebuffer is not the logical screen.** An OS resize changes the
  framebuffer only; a script window-size change moves the other way, through
  `take_window_resize`.
* **Two engines in one process do not interact.** Every field belongs to the
  engine.

---

## 5. Data flow

```
platform surface event ──► Engine::resize ──► surface_size
script window size    ──► scene window client ──► take_window_resize ──► composition root
                                                     │
                                                     ▼
   L1 input events ──► (composition root maps device→VM) ──► InputState
                                                     │
  Engine::frame(now) ─┬─► sync_presentation ─► PixelTranslation::fit
                       ├─► TimerState::advance ─► timer callbacks ─► L3 onTimer posts
                       ├─► GameFrame::before_render ─► vm.tick + L3 event dispatch
                       ├─► overlay refresh
                       ├─► upload dirty bitmaps ─► texture table
                       ├─► project_scene_report ─► RenderScene (VM px)
                       ├─► set root.transform = Fit presentation
                       ├─► GameFrame::after_project(drawn_layers)
                       └─► render::render ─► one presented frame
```

---

## 6. Failure modes

| failure | behaviour |
|---|---|
| the frame hook returns an error | `EngineError::Frame`; the frame is abandoned without projecting or drawing |
| the overlay fails to refresh | `EngineError::Overlay`; the frame is abandoned |
| a bottom render action fails | `EngineError::Render` (a transparent `RenderError`) |
| the backend presents nothing and names no surface event | the frame is recorded in the stall as that cause, warned once for the run, and the stall reports its age while it lasts |
| the display does not release a swapchain image within the presentation budget | the backend skipped the frame before submitting; the engine records it in the stall (warned once for the run, age reported at one second and then every ten), the resume line names the cause and the span, and the VM, timers and audio keep running |
| the surface is lost | the surface and swapchain are rebuilt in L1; the scene, textures and VM stay in place; the recovered frame is skipped |
| the surface is out of date | the cause is recorded in the stall and logged once for the run; the backend rebuilds the swapchain at the start of the next frame |
| a bitmap has a zero dimension or a wrong pixel length | the bitmap is skipped with a warning; the frame continues |
| a texture cannot be created for a bitmap | the bitmap is skipped with a warning; the frame continues |
| a texture batch upload fails | the bitmaps stay dirty and the next frame retries them |
| the VM size and the framebuffer size cannot be fitted | the translation is `None`; the frame draws nothing, joins the current stall under that cause, and the engine warns **once** until the sizes fit again |

No failure is swallowed into a default value: each one is a typed error, a
warning with a named cause, or a counted skipped frame.

---

## 7. Current limits

* **The mixer, the video pump and the non-self-updating transition pumps run
  inside the `GameFrame` hook**, not inside L2; the self-updating
  transitions advance from the `after_project` hook, which sees the
  projection's draw report.
* **`Engine::run(clock, hooks)` does not exist**: the platform loop still
  owns the event pump and drives `Engine::step`.
* **`TimerState::advance` is public**; it runs only inside `Engine::frame`.
* **The framebuffer is not a readback source.** `getPixel`, `piledCopy` and
  `saveLayerImage` read the CPU layer bitmap, which is VM-exact.

---

## 8. Tests

* `crates/engine/tests/engine.rs` — the frame: clock behaviour, projection
  cadence, upload retry, presentation faults, a skipped frame still running
  the hook and its due timers, and the window-resize handoff.
* `crates/engine/src/engine.rs` `mod tests` — a failed upload leaves every
  bitmap dirty; a resize event reports the extent the surface accepted; a
  frame the backend did not present is reported; a frame the display never
  released is a reported skip; a lost surface is reported as the surface fault
  that recovery owns; a stall keeps every distinct cause and reports its span,
  reports its age on a bounded schedule, and names each cause once.

---

## 9. See also

| topic | document |
|---|---|
| the frame contract and the upward hook | [`arch/l2.md`](../../arch/l2.md) §9 · [`rules/arch_rules.md`](../../rules/arch_rules.md) §4 |
| the presentation mapping | [translate.md](translate.md) |
| the projection | [projection.md](projection.md) |
| the timer contract | [timer.md](timer.md) |
| the L2↔L1 surface this object drives | [`arch/l1.md`](../../arch/l1.md) §3 |
| the archived design trail | [`docs.old/migration/12-layer-tree-and-frame.md`](../../../docs.old/migration/12-layer-tree-and-frame.md) §6 |

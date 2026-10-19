# 11 — Presentation and the pixel-translation layer

Status: **the Fit presentation and the pixel-translation mapping are
implemented.** The presentation-scaling feature is removed, `Engine` owns the
frame entry point, projection and input go through `PixelTranslation`, and the
scene is drawn at the device resolution. Open: the remaining mixer/video pumps
in the frame hook (Phase 9), device-pixel readback and device-resolution glyph
rasterization (§7). The render-resolution/supersampling idea is documented only
and is not implemented. The logical screen is the primary layer; a script that
zooms the window changes the client size only, and the translation fits the
screen into it.

This document decides how the engine maps the scene a script thinks it is
drawing onto the framebuffer it actually draws into. It supersedes
[`04-renderer-vulkan.md`](04-renderer-vulkan.md) §6.1 (the `ScalingMode`
presentation feature), which is removed.

---

## 1. The two coordinate spaces

Two spaces must never be confused:

| space | unit | who speaks it | examples |
|---|---|---|---|
| **VM pixel** (*logical pixel*) | one pixel of the logical screen (the primary layer) | scripts (L3/L4) | `System.screenWidth`, `Layer` rects, `Bitmap` dimensions, `Font.size`, `getPixel` coordinates |
| **window pixel** | one pixel of the OS window's client area | scripts (L3/L4) | `Window.innerWidth`/`innerHeight` |
| **device pixel** (*render pixel*) | one pixel of the framebuffer/swapchain | L1 | the swapchain extent, the Vulkan viewport, the CPU rasterizer's `pixels` buffer |

The reference already separates the logical screen from the window client:
`System.screenWidth` is the primary-layer size, `Window.innerWidth` is the
form's client size, and `setZoom`/`setInnerSize` change only the client
(`reference/cpp/core/base/impl/SystemImpl.cpp:877-895`,
`reference/cpp/core/visual/WindowIntf.cpp:885-903,1518-1534`,
`reference/cpp/core/environ/impl/TVPWindow.h:389-406`). The framebuffer follows
the window client. Our translation keeps the same split and additionally lets
a desktop framebuffer differ from the window client (DPI, compositor tiling).
A script still sees, and still hands back, **VM pixels** for the screen; a
window-size value is the client. Both are script-observable and reference-exact
(D34).

## 2. The presentation rule: Fit with letterbox, only

There is exactly one presentation:

* scale the VM scene **uniformly** by
  `s = min(device_w / vm_w, device_h / vm_h)`;
* place it **centred**, so the letterbox margin on the free axis is
  `(device - vm · s) / 2` for each axis;
* clear the margin to the background colour (`RenderScene.background`, opaque
  black by default — the clear L1 already performs).

There is **no** `game` / `fill` / `expand` mode and no configuration key. The
enum is deleted, not hidden. (For the record: `fill` = cocos2d `EXACT_FIT`
(`reference/cpp/core/environ/cocos2d/AppDelegate.cpp:70`), `expand` = the
`MainScene::initialize` frame-aspect sizing
(`reference/cpp/core/environ/cocos2d/MainScene.cpp:436-441`); both are dropped
by user decision. `fit` is cocos2d `SHOW_ALL`, `AppDelegate.cpp:73-75`.)

Degenerate inputs are not scaled around:

* a zero VM dimension or a zero device dimension means there is no valid
  mapping; the translation is unavailable and the frame is not presented (a
  typed error / skip), never a divide-by-zero or a synthetic `1.0` scale (D11).
* the scale is computed once per size change from the **integer** sizes, so it
  cannot accumulate across frames.

## 3. Where the translation lives

```
L4/L3   VM pixels   ── natives, layout, getPixel, input hit-test ──┐
                                                                    │  PixelTranslation
L2      owns the frame (D26) and the translation  ◄─────────────────┘  (to_device / to_vm)
        projects the scene, emits device-space nodes, reads back
L1      device pixels ── Vulkan / CPU rasterizer / swapchain ────────►
```

* **L3 and scripts never see a device coordinate.** Every value L3 reads
  (`System.screenWidth`, `Layer` extents, `Font.getTextWidth`, `getPixel`) is
  VM space; every hit-test input L3/its event adapter receives is VM space.
* **L1 never sees a VM coordinate.** The `RenderScene` handed to the pipeline
  is in device space; the readback surface the pipeline fills is device space.
* **L2 owns the mapping** and is the only layer that names both spaces. This
  is the "translation layer between L2 and L3" the user asked for: it is an L2
  value, consumed at the L2↔L3 boundary.

The layer rules are unchanged (02 §1): this adds no edge. L3 → L2 only.

This is not a foreign concept imposed on the reference — the reference itself
composes through a **draw device** (its GPU `DrawDevice`/`D3DAdaptor` path)
rather than a CPU layer pyramid, and only materializes CPU pixels when a script
reads them back. The translation layer is that same split for a second
coordinate space: the composite stays on the GPU at device resolution, and the
CPU-side pixel access is a *view* onto it (D40, §7), not a second buffer.

## 4. The scaling feature is removed

The tree once carried a first attempt at scaling (the `feat(scaling)` line and
its follow-ups). It scaled the *composited scene* through a whole-tree
`ViewTransform` and derived the logical size from the framebuffer; it is
removed, not adapted. `ScalingMode`, `PresentationScaling`, the `scaling`
configuration key, `RenderScene.view`, the `--scaling` flag and
`platform::native_size` do not exist in the tree.

Three look-alikes are **not** the removed feature, and deleting them would
break unrelated contracts:

* `api::host::Presentation` — the L1 `render` outcome (suboptimal / out-of-date
  status returned by `RenderPipeline::render`), unrelated to
  `engine::projection::Presentation`;
* `HostWindow::{logical_size, set_logical_size}` — a platform-window
  capability, not the removed logical-size plumbing;
* `Layer.setViewTransform` / `translateViewTransform` / `resetTransform` — GDI+
  `Matrix` methods, unrelated to the removed `ViewTransform`.

The fixes that landed around the scaling commits are independent and stay:
`render::scene::{node_spaces, place_rect}` (the shared implementation both
backends use, exercised through an explicit `Affine2` root transform) and the
null backend's CPU nearest-texel sampling
(`reference/cpp/core/visual/RenderManager.cpp:2902`), which keeps the software
correctness baseline reference-exact while the Vulkan sampler filters
bilinearly (D34 rasterization).

## 5. Frame ownership (D26)

**Landed: the frame entry point, clock, timer registry and projection/render
ownership.** `Engine::step`/`frame` run the frame, `GameFrame::before_render`
is the single upward hook, and `GameHost` no longer owns a `dt` or triggers
the projection. The mixer, video and transition pumps still run inside the
hook; moving them is the rest of Phase 9 (see `07`).

The translation is computed per size change *inside the frame*, so the frame
must be L2's first (D26). The contract is already written down —
[`02` §3](02-target-architecture.md) and [`03` §8](03-middle-layer-api.md) —
and is restated here only as the boundary the translation depends on:

```rust
pub struct Engine { /* L1 objects + ALL L2 state */ }

impl Engine {
    /// The whole frame. Deterministic: tests drive it with a virtual clock.
    pub fn frame(&mut self, now_ms: u64, hooks: &mut dyn GameFrame)
        -> Result<FrameReport, EngineError>;
    /// Sugar: add `dt_ms` to the engine clock and run one frame.
    pub fn step(&mut self, dt_ms: f32, hooks: &mut dyn GameFrame)
        -> Result<FrameReport, EngineError>;
    // Engine::run(clock, hooks) is the loop sugar, still open.
}

/// Declared in L2, implemented by the composition root.
pub trait GameFrame {
    /// L4 `vm.tick` plus L3's event dispatch and polls.
    fn before_render(&mut self, now_ms: u64, dt_ms: f64) -> Result<(), String>;
}
```

What this requires, and why the translation waits for it:

* **one clock (landed).** `TimerState::advance` is called only by the frame;
  the second L3 clock (`VisualState.clock`) and `app::GameHost`'s `now_ms`/`dt`
  are gone. The translation has no clock of its own — it changes only on a
  size change.
* **one size owner.** `Engine` learns the **device size** from the L1 surface
  event and the **VM size** from the logical screen (the primary layer),
  computes the translation, and stores it. Nothing above L2 holds either size;
  `app::Host::resize` only forwards the surface size to `Engine::resize`.
* **the projection seam stays L2's.** `Engine` projects the scene into the
  device-space `RenderScene` and calls `render`; it is the place the
  translation is applied.

Schedule: [`07` Phase 9](07-roadmap.md).

## 6. The pixel-translation layer

**Landed.** `engine::translate::PixelTranslation` is the pure mapping;
`Engine` refits it on a VM or framebuffer size change and rewrites the
projected nodes into device space, and the composition root maps pointer/
touch input back with `to_vm_point`.

A new L2 value (module name provisional: `engine::translate`) that owns the
VM↔device mapping. It is pure, `Copy`, and holds no clock and no cache:
`Engine` recomputes it whenever the VM size or the device size changes.

```rust
/// The VM↔device mapping of one presentation (Fit with letterbox, §2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelTranslation {
    /// VM → device uniform scale (`min` fit).
    pub scale: f32,
    /// Device-space position of the VM origin (the letterbox margin).
    pub offset: (f32, f32),
    /// The size the scene declares in VM pixels.
    pub vm_size: (u32, u32),
    /// The framebuffer size in device pixels.
    pub device_size: (u32, u32),
}

impl PixelTranslation {
    /// Fits `vm_size` inside `device_size`. A zero dimension is an error (D11).
    pub fn fit(vm_size: (u32, u32), device_size: (u32, u32))
        -> Result<Self, TranslateError>;

    // ---- L2 → L1 (projection): VM to device ----
    pub fn to_device_point(&self, p: (f32, f32)) -> (f32, f32);
    pub fn to_device_rect(&self, r: RectF) -> RectF;

    // ---- L1 → L2 → L3 (input): device to VM ----
    pub fn to_vm_point(&self, p: (f32, f32)) -> (f32, f32);

    // ---- rasterization resolution (§6.2) ----
    /// Device pixels per VM pixel — what a glyph atlas is rasterized at.
    pub fn raster_scale(&self) -> f32;

    // ---- readback (§7) ----
    /// The device region a VM pixel covers.
    pub fn vm_pixel_region(&self, x: u32, y: u32) -> PixelRegion;
}
```

### 6.1 The projection

`Engine::render` (or the projection step `frame` calls) converts each drawn
node's VM-space destination rectangle to device space with `to_device_rect`,
and hands the pipeline a device-space `RenderScene`. The whole-tree root
transform is `Affine2::scale(s, s).then(translation(offset))` — exactly the
`record_scene(..., root)` capability that already exists. Layers keep their
own `transform` in VM space; the root transform composes last.

### 6.2 Font rasterization (the decoupling the user asked for)

**Open.** Today text is rasterized into the VM-resolution layer bitmap and the
GPU scales that bitmap; the visible difference at `s > 1` is softness, which
D34 permits. Layout metrics are already VM and unchanged.

Scripts think text is composed on the CPU at `Font.size` VM pixels. In the
target it is rasterized at `size_px * raster_scale()` device pixels (crisp on
a HiDPI screen) and placed through the device transform:

* **layout metrics stay VM** — advances, extents, `Font.getTextWidth`,
  `getEscWidth` and every script-visible measurement are computed in VM units
  and returned unscaled (D34). Only the rasterized coverage and its placement
  are device-space;
* the glyph atlas is keyed in device units (raster size), so a scale change
  rebuilds it; a colour change still does not (the coverage/tint split).
* This is the "what the VM thinks is CPU-composed is actually composed on the
  GPU" boundary: L3 keeps calling the L2 text engine in VM units; L2 decides
  the device rasterization. `compat` does not learn about device pixels.

### 6.3 Input

**Landed.** The composition root (`app::GameHost`, the same `VmHook` that runs
the VM tick) maps each pointer/touch position device→VM with
`PixelTranslation::to_vm_point` **before** `vm.tick`, so L2 input state and the
L3 hit-test only ever see VM coordinates. The mapping itself is the L2 value;
the composition root only applies it. `map_input_to_logical`'s old
`ViewTransform` plumbing is gone. Keyboard, wheel, text and focus events carry
no position and pass through unchanged.

### 6.4 Boundaries the translation must respect

* **L2 embeds it; L3 never sees it; L1 never names it.** L1 stays device-only
  (D10/D33). Every L2 render/pixel method takes VM coordinates and applies the
  mapping internally; L3 natives keep their VM-coordinate contract and do not
  call `to_device_*`/`to_vm_point`/`raster_scale` or name a device pixel. The
  composition root only maps platform input through an L2 method. This is
  D41.
* **No `ViewTransform` in `render`.** The pipeline keeps the generic root
  transform (`Affine2`) and nothing about a "presentation"; that vocabulary is
  L2/L3.
* **No new config.** Fit is not selectable; there is no `scaling` key on any
  launcher, desktop or Android (see §4).

## 7. Device scaling and readback

**Device scaling and input mapping landed; readback and glyph rasterization
are open.** The projection is device-space, so the scene is already drawn at
the framebuffer resolution. The rest of this section is the unbuilt readback
path; today `getPixel`/`piledCopy`/`saveLayerImage` read the CPU layer bitmap,
which is VM-exact.

The single backing is the **device-resolution** framebuffer. The engine does
**not** keep a VM-resolution copy and does **not** downscale the frame back to
one. Readback scales the **access**:

* a VM coordinate `(x, y)` addresses the device region
  `[x·s, (x+1)·s) × [y·s, (y+1)·s)` — `PixelTranslation::vm_pixel_region`;
* at `s == 1` this is the reference's exact pixel;
* at `s > 1` the region is reduced to the single VM value a script expects; the
  reduction is a **documented extension** (the reference has no second space),
  recorded per D34 as a rasterization/readback approximation, never as a
  script-observable divergence at the reference's own scale.

### The transport is a capability, and on Android it is `AHardwareBuffer`

The region read does not round-trip the whole frame through the CPU. There are
two transports, returning the same bytes:

* **Android — `AHardwareBuffer`.** The device backing is allocated *as* an
  `AHardwareBuffer` (`GPU_COLOR_OUTPUT | CPU_READ_OFTEN`), imported into
  Vulkan, rendered into, and read with `AHardwareBuffer_lock` over the region.
  It is **not** a Vulkan-allocated image exported for CPU access (the extension
  gives no reliable CPU-visible path in that direction) and **not** a raw
  `mmap` (gralloc may tile or compress; only the lock does the cache and
  reformat work). Mechanics, stride, region scoping, the dmabuf fd and the
  device-validation list: [`10-android-backends.md`
  §"Device-pixel readback"](10-android-backends.md).
* **Desktop and fallback — a host-visible staging copy.**
  `vkCmdCopyImageToBuffer` the region into a `HOST_VISIBLE | HOST_COHERENT`
  buffer, wait, map. Always available.

The transport is **probed, not guessed**: a typed error (D11) when the chosen
transport cannot run on this device, and the other transport otherwise. That is
a capability choice (the same bytes over a different path, as FFmpeg vs
MediaCodec, D5), not a silent semantic fallback.

Consumers: `Bitmap.getPixel`, `piledCopy`, province images, `saveLayerImage`,
and transition/`getPixel`-driven blends — every one of them a VM-space call at
the L2↔L3 boundary, and each targeting either the frame backing or a
GPU-composed layer buffer whose CPU copy has been discarded.

## 8. Status

| change | state |
|---|---|
| remove the scaling feature (§4) | landed; one-to-one presentation, no scaling symbol left |
| L2 owns the frame (D26, §5) | frame entry point, clock and projection landed; mixer/video pumps still in the hook (Phase 9) |
| `PixelTranslation` in L2 (§6) | landed; behaviour-neutral at `s == 1` — identical output on every existing test |
| device scale + input inverse map (§6.2–6.3) | landed; the scene draws at the device resolution and hit-tests in VM pixels |
| device-pixel readback + device-resolution glyph rasterization (§7) | open |

The mapping is deliberately **behaviour-neutral at scale 1**: with
`vm_size == device_size`, `scale == 1` and `offset == (0,0)`, so projection and
input reduce to the unscaled path and the surface lock, parity reports and
golden tests are unchanged. The device scale switches on by itself when the
window client and the logical screen differ (the game's own window zoom, the
Android surface, or a desktop window the player resized).

## 9. Open items

1. **`getPixel` reduction at `s > 1`** (only for a GPU-composed backing).
   Point-read the region origin, or average the box? The origin is exact at
   `s == 1` and cheapest; averaging needs a defined rounding. Today readback
   comes from the VM-resolution CPU layer bitmap, so the question waits for
   the GPU readback path.
2. **Readback transport.** Android reads over `AHardwareBuffer_lock`
   (region-scoped, stride from `lock_and_get_info`); the desktop and the
   fallback use a host-visible staging copy. Open: the device behaviour of
   `CPU_READ_OFTEN`, which tiling modifier gralloc picks, and how much `lock`
   stalls the GPU — see [`10`](10-android-backends.md) "Device-pixel readback"
   and its device-validation list.
3. **Aspect mismatch.** Fit letterboxes; a device whose aspect is *taller*
   than the VM's letterboxes vertically. Confirm the reference's own
   letterbox colour and whether the game ever sets a non-black background
   (`RenderScene.background` is the current answer).
4. **What must be device-resolution and lockable.** Only the frame backing needs
   the swapchain blit; offscreen canvases and GPU-composed bitmaps may not. If
   most readback targets are bitmaps whose CPU copy we keep (`D8`), the
   AHB-backed frame may be unnecessary — decide with a readback-frequency
   measurement, not up front (`10-android-backends.md` "Device-pixel readback").

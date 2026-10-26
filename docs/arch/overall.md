# Architecture — overall

Status: **authoritative**. This directory describes the tree as it is today.
The numbered decisions live in [`../migration/README.md`](../../docs.old/migration/README.md)
§3; the working rules for humans and agents live in
[`../../AGENTS.md`](../../AGENTS.md).

krkr-rs is a native Rust reimplementation of the KiriKiri2 (TVP) engine. It is
a **compatible emulator**, not a port: the goal is that unmodified games and
scripts behave exactly as they do on the reference engine, and
`reference/cpp/**` is the oracle for behaviour.

| Document | Contents |
|---|---|
| [`overall.md`](overall.md) | The layers, the dependency direction, the crate map, the coordinate spaces, the frame in one page, and how the rules are enforced. |
| [`l1.md`](l1.md) | L1 — the platform core: stateless services (fs, media, render, platform) and the `api` DTO crate. |
| [`l2.md`](l2.md) | L2 — the engine: the layer tree, the projection, the presentation mapping, the frame, storage, sound, video, text, timers, transitions. |
| [`l3.md`](l3.md) | L3 — the compatibility surface: the script classes, the engine seam, KAG, the event adapter, the plugins, the surface lock. |
| [`l4.md`](l4.md) | L4 — the TJS VM host over the vendored C++ VM. |

---

## 1. The four layers

```
┌──────────────────────────────────────────────────────────────────────┐
│ L4  vm       TJS VM host (Tjs2Engine, GC, bytecode, script cache)     │
├──────────────────────────────────────────────────────────────────────┤
│ L3  compat   KiriKiri compatibility surface: native classes, KAG,    │
│              event adapter, static plugins, Win32 emulation          │
├──────────────────────────────────────────────────────────────────────┤
│ L2  engine   a small, generic game engine: the layer system/scene    │
│              graph, bitmaps, sound mixer, timers, VFS namespace;      │
│              owns engine state                                        │
├──────────────────────────────────────────────────────────────────────┤
│ L1  platform api · fs · media · render · platform                    │
│              stateless services + all platform code (window, event    │
│              loop, input, clock, threads, Vulkan, FFmpeg/MediaCodec,  │
│              raw IO + XP3 parse)                                      │
├──────────────────────────────────────────────────────────────────────┤
│ app          composition root: select platform, wire L1..L4, run loop │
└──────────────────────────────────────────────────────────────────────┘
```

(The desktop root is `crates/app`; the Android root is `crates/android`.
Both are wiring only.)

The dependency direction is strictly downward, with exactly one extra edge:

```
L4 vm      → compat(L3), platform(L1)
L3 compat  → engine(L2), platform(L1), api, fs, tjs
L2 engine  → L1 type crates (api, fs, render)
L1         → OS / Vulkan / FFmpeg / MediaCodec only
app        → everything (wiring only)
```

* `L4 → L1` exists because the VM host needs the platform abstraction (clock,
  threads, bytecode/state IO) without going through the compat surface.
* **L1 must not know about layers, TVP, TJS, storages or KAG.** It is
  mechanism only.
* **L2 must not contain TVP/TJS vocabulary** — no `LT_*`, no `TVP_*`, no
  virtual-key codes, no `.sli`, no `0..100000` volume scale, no KAG tags.
  Generic concepts only: `BlendMode`, a normalized gain, an opaque key code.
* **L3 must not contain platform code** — no `#[cfg(target_os = …)]`, no
  `winit`, no `android-activity`, no `std::fs`, no JNI. It uses L1
  abstractions.
* **L3 names the engine only through `crates/compat/src/engine_api.rs`.**
  That file is the L3↔L2 protocol surface; the `l3-engine-api` rule in the
  `layering` gate check rejects `engine::` anywhere else under
  `crates/compat/src`.

### The three load-bearing rules

1. **L1 is stateless.** An L1 object is a facade over a platform capability.
   Its methods are `&self` and take the evolving state as an explicit `&mut`
   value (`RenderState`, `FileState`, `DecoderState`, …) that L2 owns. No
   `static mut`, no `thread_local!`, no `LazyLock`/`OnceLock`/`OnceCell`, no
   process-global registry, no hidden cache. Enforced by `krkr-gate stateless`.
2. **L1 owns traversal and rendering.** L2 maintains the tree and calls
   `render`; it never builds a draw list, never sorts, never selects a blend
   state. Enforced by review and by the shape of the projection seam
   ([`l2.md`](l2.md) §3).
3. **L3 is thin.** Every L3 native member reduces to *marshal args → one L2
   call → marshal result*. Real logic belongs in L2 unless it is genuinely
   TVP-specific; and TVP specifics never move into L2 just to shrink L3.

### The genericity test

For any behaviour, ask *does this need TVP vocabulary?*

| question | answer |
|---|---|
| generic scene graph, bitmaps, mixing, timers, VFS, projection | **L2** |
| `arc.xp3>path`, masks, province, `Layer.type`, focus/hit-test, `Font.face`, KAG tags, `.sli`, the `0..100000` volume scale | **L3** |
| platform host, window, event loop, raw IO, decode, Vulkan | **L1** |

Two-sided: if moving a behaviour into L2 requires naming a TVP concept, it
stays in L3; if an L3 file grows larger than the L2 module it wraps, the
abstraction is leaking and the generic behaviour moves down.

---

## 2. Crate map

| crate | layer | role | key items |
|---|---|---|---|
| `crates/platform/api` | L1 | shared DTOs + typed error enums, no impls | `BlendMode`, `RectF`/`PixelRegion`, `FontMetrics`, `FsPath`, `InputState`, `SurfaceEvent`, `FsError`/`MediaError`/`RenderError`/`GlyphError`/`AudioError` |
| `crates/platform/fs` | L1 | stateless `FileSystem`/`FileState` + XP3 read/parse | `FileSystem`, `FileState`, `fs::xp3::Xp3Archive`, `normalize_in_archive_name` |
| `crates/platform/media` | L1 | stateless `Media` + image/audio/video decode | `decode_image`, `open_audio`/`read_audio`, `open_video`/`present_video`/`video_audio`, `tlg`, `yuv` |
| `crates/platform/media/psd` | L1 | stateless PSB container + PSD image decode | `psd::container`, `psd::decode` (separate workspace member; excluded from the `media` package) |
| `crates/platform/render` | L1 | stateless `RenderPipeline`/`FontSystem` + Vulkan | `RenderPipeline`, `RenderState`, `RenderScene`, `Texture`, `NullRenderPipeline`, `render::scene::node_spaces` |
| `crates/platform/platform` | L1 | window/surface, event loop, input, clock, threads | `PlatformWindow`, `PlatformEvent`, `InputEvent`, `AudioDevice`, `Clock`/`Ticker`, `FramePacing` |
| `crates/engine` | L2 | the generic game engine; owns engine state | `Engine`, `Scene`, `projection`, `translate::PixelTranslation`, `storage::Storage`, `sound::Mixer`, `text::TextEngine`, `timer::TimerState`, `video::VideoOverlay`, `trans::Trans` |
| `crates/compat` | L3 | native classes + KAG + event adapter + plugins | `CompatHost`, `CompatCtx`/`CompatServices`, `engine_api` (the seam), `natives::*`, `null_plugin` |
| `crates/vm` | L4 | TJS VM host over `tjs` | `VmHost`, `FrameCtx` |
| `crates/tjs` | vendored | the C++ TJS2 VM, all-FFI | `Tjs2Engine`, the native-class builder, the `tjs2_*` C surface |
| `crates/app` | root | the desktop composition root | `Host`, `GameHost`, `VmHook`, `run` |
| `crates/android` | root | the Android composition root | `android-activity` `GameActivity` loop, JNI wiring, `krkr.log`, `krkr_state.json` |
| `crates/testkit` | dev | shared test helpers (dev-dependencies only) | — |

The `app` and `android` crates are the two composition roots. They contain
**wiring only** — no platform logic, no engine logic, no game semantics.

---

## 3. The three coordinate spaces

Three pixel spaces exist and must never be confused.

| space | unit | who speaks it | examples |
|---|---|---|---|
| **VM pixel** (logical pixel) | one pixel of the logical screen (the primary layer) | scripts, L3, L4 | `System.screenWidth`, layer rects, `Bitmap` dimensions, `Font.size`, `getPixel` coordinates |
| **window pixel** | one pixel of the OS window's client area | scripts, L3, L4 | `Window.innerWidth` / `innerHeight` |
| **device pixel** (render pixel) | one pixel of the framebuffer/swapchain | L1 | the swapchain extent, the Vulkan viewport, the CPU rasterizer's buffer |

The reference already separates the logical screen from the window client
(`System.screenWidth` is the primary-layer size, `Window.innerWidth` is the
form's client size). The engine keeps that split and adds the device space
on top.

**Only L2 names more than one of them.** `engine::translate::PixelTranslation`
owns the VM↔device mapping; L3 never sees a device coordinate, and L1
receives the VM-space render tree with an opaque root transform but never
interprets the VM semantics. Every value a script can observe stays in VM
pixels and reference-exact. See [`l2.md`](l2.md) §4.

---

## 4. The frame in one page

The engine owns the frame. `engine::Engine::run(clock, hooks)`,
`Engine::step(dt_ms, hooks)` and `Engine::frame(now_ms, hooks)` are the frame
entry points; nothing above L2 owns a `dt`. The loop-level entry point is
`run`: the caller injects the frame's elapsed time through its clock source
once, and every timeline the frame derives comes from that value.

```
Engine::frame(now_ms, hooks)
  ├─ set the clock; fit the presentation (PixelTranslation)
  ├─ deliver the due timers                    → callbacks post onTimer events
  ├─ hooks.before_render(now_ms, dt_ms)       → L4 vm.tick + L3 event dispatch
  │                                              and the staged pumps
  ├─ fit the presentation again                → a script may have resized
  ├─ refresh the render-only overlay
  ├─ upload dirty bitmaps; prune stale textures
  ├─ project the L2 scene into the VM-space RenderScene
  ├─ put the Fit presentation on the render-scene root
  ├─ hooks.after_project(drawn_layers)        → self-updating transitions
  ├─ upload + project again if the hook changed the scene
  └─ one L1 RenderPipeline::render call
```

The single upward edge is the `GameFrame` trait declared in L2 and implemented
by the composition root (`app::VmHook` runs the VM tick and the audio drain;
`compat::run_frame_body` drains the timer posts the engine produced and runs
the sound, video, async-trigger, continuous-handler and movie/transition
polls).

The mixer, video and non-self-updating transition pumps still run inside that
hook — moving them into L2 is open work (see [`l2.md`](l2.md) §11).

Presentation is one rule: **fit with letterbox**. The scene is scaled
uniformly by `min(device/vm)`, centred, and the margin is cleared. There is
no mode enum and no configuration key on any launcher.

---

## 5. The two seams

```
        L3 compat                          L2 engine                    L1 platform
   script classes  ──engine_api.rs──▶  Scene · Storage · Mixer · …  ──▶  FileSystem · Media · RenderPipeline
   (VM pixels)                        (VM pixels, owns state)        (device pixels, stateless)
```

* **L3 ↔ L2** — `crates/compat/src/engine_api.rs` re-exports the engine
  items the compatibility layer codes against. It is a re-export, not a
  wrapper: a protocol type *is* the engine type. The full inventory, with the
  ownership, coordinate and error contract of each group, is in
  [`l3.md`](l3.md) §2 and [`l2.md`](l2.md) §10.
* **L2 ↔ L1** — L2 calls the stateless L1 services directly and owns every
  state value it passes in. L3 never names an L1 service type.

---

## 6. How the rules are enforced

The gate is the mechanical enforcement of this architecture.

```bash
uv run --project scripts krkr-gate gate     # the ordered static gate
uv run --project scripts krkr-gate test     # the test suites
```

| check | what it holds |
|---|---|
| `stateless` | L1 has no mutable statics, no `thread_local!`, no lazy statics, no id-keyed registries, no `&mut self` on an L1 service. |
| `layering` | L1/L2 vocabulary rules, no platform code in L3, the `l3-engine-api` seam rule, `unsafe` placement and `SAFETY:` notes, the `missing_docs` lint. |
| `error-handling` | no silent fallback: no `unwrap_or` on a real error, no dropped `Result`, no pointer-sign tests on a script-visible handle. |
| `house-style` | comments describe the code as it is; no external-engine citation or history narration outside tests; no `#[allow(clippy::…)]`. |
| `storage-boundary` / `storage-media` | the storage/media boundary between L1, L2 and L3. |
| `native-construction` / `native-arity` / `callback-objthis` / `void-getter` / `lifecycle-order` | the shape of the L3 native surface. |
| `surface-lock` | the live VM dispatch surface equals the committed baseline — any added or removed member fails the gate. |
| `native-parity` / `plugin-parity` | coverage *reports* against the reference; presence, not correctness. |
| `host-facts` / `android-target` | the host/platform contract, and the per-target dependency graph. |

The **surface lock** is the contract for L3's dispatch surface; behaviour is
constrained separately, by citing the reference in every behaviour test.
See [`l3.md`](l3.md) §7.

---

## 7. Where to read more

| topic | document |
|---|---|
| the layers, the crate map, the frame, the coordinate spaces | this file, [`l2.md`](l2.md) §9 |
| stateless L1, the object/state split | [`l1.md`](l1.md) §1 |
| the layer tree, the projection, cache/clip/order | [`l2.md`](l2.md) §2–§3 |
| the L2 public API: the engine crate's types, operations and error types | [`l2-api.md`](l2-api.md) |
| presentation and the pixel-translation layer | [`l2.md`](l2.md) §4 |
| the render pipeline, blend slots, the CPU baseline | [`l1.md`](l1.md) §3 |
| media backends (FFmpeg / MediaCodec), the PSB/PSD reader | [`l1.md`](l1.md) §4–§5 |
| the VM/compat protocol, the engine seam | [`l3.md`](l3.md) §2 |
| plugins, the null plugin, the DLL-surface pipeline | [`l3.md`](l3.md) §5 |
| the VM host, script loading, the Rust-port audit | [`l4.md`](l4.md) |
| Android backends (AAudio, MediaCodec, `AHardwareBuffer`) | [`../android.md`](../../docs.old/android.md), [`l1.md`](l1.md) §6 |
| open work | [`../../TODO.md`](../../TODO.md) |
| native and plugin parity reports | [`../native_parity.md`](../native_parity.md), [`../plugin_parity.md`](../plugin_parity.md) |
| runtime configuration and launcher options | [`../runtime-config.md`](../../docs.old/runtime-config.md) |

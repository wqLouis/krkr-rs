# krkr-rs architecture migration — middle layer + Vulkan core

Status: **design; partially implemented.** The docs are the source of truth;
see [`07-roadmap.md`](07-roadmap.md) for what has landed.
Branch: `migration/middle-layer-ash`
Created from: `main` (`7ba9435 docs: removed TODO.md`)

This directory is the design + investigation record for the architecture
rework described below. Nothing in `crates/` is touched by this branch; the
only commits are documentation.

---

## 1. Why we are doing this

The current engine is built **inside Bevy**. Bevy's ECS, asset pipeline,
scheduling, windowing and input are used as the engine's substrate, not as a
replaceable backend:

* the game loop is a Bevy `Update` system chain
  (`render/src/runner.rs` `run_vm → … → sync_scene`);
* the logical layer tree is projected straight into Bevy entities, materials
  and sprites every mutating frame (`render/src/sync.rs`);
* the menu is Bevy UI (`render/src/menu.rs`);
* the Android entry point is `#[bevy_main]`
  (`crates/android/src/lib.rs`).

That is fine for a prototype, but it makes three things hard:

1. **Correctness against TVP.** KiriKiri's object model is an
   object-oriented layer/window/bitmap tree with CPU pixel access
   (`getPixel`, `piledCopy`, province images, masks, `MainImage` read-back).
   An ECS is a poor fit for owning that tree; we keep fighting the impedance
   mismatch and re-deriving the tree on every sync.
2. **Rendering control and cost.** Bevy + wgpu drags in OpenGL/Metal/WebGPU
   backends and an asset/material/entity pipeline we do not need. The actual
   GPU workload is a small set of textured quads with fixed blend states
   (`render/src/blend.rs`). We want a thin, explicit Vulkan pipeline.
3. **Platform surface.** Bevy's windowing/input/UI is the top of the
   dependency tree. Replacing any one piece (UI, input, surface) means
   replacing all of it, and Android is locked to Bevy's GameActivity glue.

The reference C++ engine already separates these concerns
(`reference/cpp/core/io/IFileBackend`, `core/visual/RenderManager`,
`core/movie/ffmpeg`, `core/sound`), and the user's target design restores
that separation in Rust.

## 2. The target in one picture

```
┌──────────────────────────────────────────────────────────────────────┐
│ L4  TJS VM engine        tjs (C++ VM) + VM host, GC, bytecode    │
│     top layer; drives the L3 natives, uses the L1 platform abstraction│
├──────────────────────────────────────────────────────────────────────┤
│ L3  KiriKiri compatibility  native classes + KAG + event adapter +    │
│     static plugins; composes L2 primitives into the script API        │
├──────────────────────────────────────────────────────────────────────┤
│ L2  Middle layer — a small, generic game engine (engine)         │
│     the layer system (scene graph), bitmaps/sprites, audio/video,     │
│     input, timers, VFS namespace; owns engine state; not TVP-specific │
├──────────────────────────────────────────────────────────────────────┤
│ L1  Platform core                                                     │
│     renderer (Vulkan) · media (ffmpeg|mediacodec) · fs + XP3 parse ·  │
│     window/host/event loop/input/clock/threads — ALL platform code,   │
│     selected at compile time, behind one platform abstraction         │
└──────────────────────────────────────────────────────────────────────┘
```

The **middle-layer API** is the contract between L2 and L1: L1 creates the
abstraction objects (`RenderPipeline`, `Media`, `FileSystem`, `FontSystem`,
`AudioDevice`, `Platform`) and provides the mechanism — including the entire
render pipeline and all platform code. L2 stores every **state value**
(`RenderScene`, `RenderState`, `SurfaceState`, `DecoderState`, `FileState`,
the layer system / scene graph, caches, indices) and performs actions on
those objects. L2 is a **generic game engine**: it owns the engine state
(including the layer system) but no TVP semantics. Everything
KiriKiri-specific — the script-visible classes, KAG, masks, province,
storage naming — is L3, composed from L2 primitives. L1 does **not** own
layers or engine state; it only computes, decodes, renders, does file I/O
and hosts the platform loop.

The object/state split is the load-bearing rule (D9): an L1 object is a
stateless facade over a platform capability, and every evolving thing is an
explicit value that L2 holds and passes back into L1 actions. In particular
(D10) the **L1 render pipeline consumes the L2 scene/layer tree and does all
traversal, composition and drawing**; L2 only keeps the tree current. L1 has
no process-global registries, no singletons, no hidden caches, no mutable
object fields.

## 3. Decisions recorded by this plan

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Introduce an explicit, stateless middle-layer API (operations + plain DTOs) between the engine and the platform. | Decouple semantics from backends; make both mockable and testable; enable incremental migration. |
| D2 | The **layer system (the generic scene graph) lives in L2**; the **render pipeline lives in L1** and consumes the scene tree. The **TVP semantics** layered on top (masks, province, exact property names, storage naming) are **L3**. | Game engines hold scene graphs, so the generic layer system is engine state in L2. What scripts see is KiriKiri-specific and belongs to the compat layer. Traversal, blend selection, batching and drawing are mechanism → L1. |
| D3 | Replace Bevy with **`ash` (raw Vulkan)** for rendering. | Smallest GPU workload is quads + fixed blends; we want explicit pipelines and no GL/Metal/WebGPU weight. |
| D4 | Vulkan only; no OpenGL/Metal/WebGPU/WebGL fallback. | User requirement. Documented risk in `07-roadmap.md §6`. |
| D5 | Media is a bottom-layer **media engine** with two backends: system **FFmpeg** on Linux, **MediaCodec** on Android. | Matches the reference (`core/movie/ffmpeg`) and current split (`tvp-natives/src/video/{ffmpeg,android}.rs`). |
| D6 (rev.) | File system **and XP3 container parsing** are stateless L1 surfaces; the **VFS namespace** and TVP `arc.xp3>path` addressing are L2/L3. | Parsing holds no state, so it belongs in L1 with raw path/URI byte IO. The namespace/state stays in L2; the TVP naming rules are L3 (see 02 §5). |
| D7 | Strangler migration, not a big-bang rewrite. | Keep the game runnable at every step: first a Bevy-backed implementation of the new API, then swap in the Vulkan core. |
| D8 | Keep CPU rasterization/compositing as the correctness baseline; GPU acceleration is an optimization behind the same API. | Scripts read pixels back (`getPixel`, `piledCopy`, `saveLayerImage`); the pixel-exact path must survive. |
| D10 | The bottom layer owns an **abstract render pipeline**: L2 passes a `RenderScene` tree (layer objects) plus L1-owned `RenderState`, and L1 handles all traversal, composition, batching and drawing. | Keeps the render mechanism out of L2 and identical across the Bevy adapter and Vulkan core; L2 becomes a state store and dispatcher. |
| D11 | **No silent fallbacks and no hardcoded limits.** The code is expected to work on valid input; where the intended behavior cannot be performed (unsupported feature, malformed/overflowing data, unrepresentable size), L1/L2 return a typed **error**. No degraded substitutes (e.g. silence in place of failed audio), no arbitrary maximum-size caps, no "guess and continue". Structural validation (rejecting an impossible header value) is an error, not a limit. | Silent degradation hides bugs and makes behavior input-dependent; arbitrary caps break valid large assets. Errors make failures explicit and testable. |
| D9 | **L1 creates stateless abstraction objects; L2 stores all state and performs actions on those objects.** No globals, singletons, hidden caches or mutable object fields in the bottom core. Evolving state is an explicit value (`RenderScene`, `RenderState`, `DecoderState`, `FileState`, …) owned by L2; pure transforms are free functions. | Offloads the abstraction to L1 while keeping L2 the single owner of the game state. The current codebase hides engine state in process-global statics (`history/01-investigation.md §9`); the object/state split makes that impossible and keeps L1 swappable, mockable and parallel-safe. |
| D12 | **Final layering:** host/window/platform code is **L1**; **L2** is a small generic game engine (layer system/scene graph, audio/video, VFS); **L3** is the KiriKiri compatibility surface; **L4** is the TJS VM. | The loop is platform code, so it lives at the bottom; the VM sits above the compat surface; higher layers stay free of platform code. |
| D13 | Platform and backends are selected at **compile time** (`platform-linux\|platform-android`, `media-ffmpeg\|media-mediacodec`), not bundled at runtime. | Keeps `#[cfg]` inside L1 and the high layers platform-agnostic. |
| D14 | Native plugins are **statically built into L3**; every ported DLL name (and its aliases) lives in one `BUILTIN_PLUGINS` table, so `Plugins.link` recognizes it and reports the built-in — the **null plugin object** (no-ops every call, logs a warning, never throws) is installed only for a DLL we have **not** ported. | Linux/Android cannot load the Windows DLLs; throwing would make games unplayable. The single documented exception to D11, and it is not silent. |
| D15 (amended) | Game loading takes an **explicit archive list** (`--archive`, repeatable, later wins), never scans a folder; `startup.tjs` is found **inside the mounted archives**; a missing startup is a typed error. **Amendment:** the "switch back to auto scan" request was checked against the reference and declined — there is **no default folder scan** to mirror: `TVPCheckStartupArg` only inspects `argv[1]` (`reference/cpp/core/environ/win32/Platform.cpp:93-140`), Linux/macOS/Android return `false` (`environ/linux/Platform.cpp:285`, `environ/apple/macos/platform.mm:208`, `environ/android/AndroidUtils.cpp:695`), and `data.xp3`/`patch.xp3` appear nowhere in `reference/cpp`. What was added instead: a positional `argv[1]` (file = archive, directory = app path + loose `startup.tjs`) and `--base-dir` (default: the first archive's parent), which roots both relative
archive paths and loose files; relative archive paths are absolutized against
it so the app path is never re-joined. | Mirrors `TVPCheckStartupArg`/`TVPCheckArchive`; avoids fragile folder scans; works for SAF/Android. |
| D16 | **L2 provides the abstraction; L3 is a thin VM↔engine adapter — and L2 stays a *generic* engine.** L2 must be abstract enough that a native is mostly marshalling, and must not offload generic behavior up to L3; but it must express that behavior in **generic** terms (a blend is the generic `BlendMode`, a gain is normalized, a key is an opaque code) and must **never absorb TVP specifics** just to shrink L3. Two-sided test: generic behavior with no TVP vocabulary → L2; anything naming TVP (member names, VK codes, `.sli`, the 0..100000 volume scale, KAG syntax) → L3. L3 then holds only marshalling, TVP semantics, and the shared behavior structs it composes. | Keeps the engine reusable and the compat surface mechanical, without turning L2 into a TVP compatibility layer. Today L3 is ~2.25× L2 in source — the smell this fixes. |
| D17 | The **TJS VM stays C++** for now; the Rust port is **deferred and not a first priority** (see `08-tjs-vm-and-native-plugins.md` §10.7). Revisit after a real game runs end-to-end. | The C++ VM works and is tested; the port is the single largest remaining item and is not on the critical path to running games. |
| D18 | First-class **host** targets are **Android and Linux**; Windows is not a host platform. But the games are Windows-native, so **we must emulate the Win32 environment they expect** — synthetic `HWND` / draw-device / message-receiver / registry / shell / system-metric behavior in L3, so a game that probes Win32 APIs sees a plausible Windows. No real Win32 API is linked; the emulation is ours. (Original plugin **DLLs** still follow D14.) | The games assume Windows; to run them unmodified we must reproduce the environment they probe, not merely the scripting surface. A typed error there is a compatibility bug, not correct behavior. |
| D19 | **FFmpeg is a required system dependency** (linked dynamically, D5); it is **not bundled**. | Matches the reference; keeps the LGPL dynamic-link story and avoids shipping a codec blob. |
| D20 | Compatibility target is **KiriKiri 2 and KiriKiri Z**. Plugins are **ported natively when feasible**; plugins we cannot port (closed-source) fall back to the D14 null plugin. | Defines the reference surface and where the null-plugin boundary sits. |
| D21 | The L2 API-stability guard is a **`cargo public-api` snapshot** (`02-target-architecture.md` §12.6, option a). **Superseded by D33:** the snapshot is dropped; L2 stability is enforced by review and audit, not by a checked-in tool. | Cheapest, CI-friendly, freezes L2 so L1 can be optimized underneath. |
| D22 | **Performance work is deferred** until a real game runs end-to-end (`07-roadmap.md` §9); correctness first, then invest. | Premature optimization before the compatibility surface is proven risks optimizing the wrong thing. |
| D23 | **Fonts come from the system and/or the game's own XP3**; font resolution follows the TJS engine's font-loading mechanism. No bundled font and no invented fallback. | CJK titles ship their own fonts; mirroring the engine's mechanism is the only compatible behavior. |
| D24 | **Save data must be compatible with KiriKiri's format.** The legacy `main` branch is kept until this branch is tested and grounded, then merged into `main`. | Save compatibility is a user-facing feature; keeping `main` is the safety net during migration. |
| D25 | **`TimerConfig::capacity: u32` is replaced by `TimerConfig::suppressed: bool`** (accessors `TimerState::{suppressed, set_suppressed}`; `TimerError::InvalidCapacity` removed). L2 can now express a timer that is armed, enabled and on schedule yet **never fires**. | The old field was inert — it was stored, validated and returned, but never consulted when firing, so removing it changes no behaviour. `capacity` is also TVP vocabulary (`TVP_DEFAULT_TIMER_CAPACITY`), which [02 §12.2 rule 6](02-target-architecture.md) keeps out of L2. And the reference bounds *queued events*, not fires (`reference/cpp/core/utils/TimerIntf.cpp:69-100`), so a fire budget in L2 would encode the wrong semantics; `suppressed` is the generic concept L3 maps `Capacity < 0` and `interval <= 0` onto. The §3 type-level surface is unchanged. |
| D26 | **L2 owns the frame — it is the runtime.** `Engine::frame(now_ms, hooks)` advances timers (invoking their stored callbacks), mixer and video from L2's own frame clock, runs one injected `GameFrame::before_render()` hook, then projects and calls L1 render; `Engine::run(clock, hooks)` is sugar. `TimerState::advance` becomes private, `compat::run_frame_body` stops orchestrating, and nothing above L2 owns a `dt`. | L1 is stateless and a loop *is* state (time, frame count, ordering). A loop placed above L2 leaves nobody responsible for `advance → project → render` — it degrades into a convention shared between two files, which is exactly how the current drift happened — and it keeps a second clock above L2 competing with the frame clock L2 already needs. L2 reaches script code only through interfaces it declares (`GameFrame`, stored callbacks), so the dependency rule holds. Contract: [`02` §3](02-target-architecture.md); schedule: [`07` Phase 9](07-roadmap.md). |
| D27 | **`engine::timer` collapses to `create_interval(interval_ms, callback)` · `set_interval` · `set_enabled` · `remove`** (plus `advance(now)` until D26 lands, then private). `TimerConfig`, `TimerMode`, `TimerFire` and `suppressed`/`count`/`next_fire_ms`/`config`/`mode()` go; L3 decides whether to arm at all, so `Capacity < 0` and `interval <= 0` never reach L2. | Supersedes D25 — `suppressed` was the transitional form. L2's job is "wait, then call": only `Repeat` was ever used (`OneShot` appears in engine's own tests and nowhere else), and `TimerMode` collided with the script's `mode`, which means *event priority* (`EventIntf.h:312`) and never controls repetition. Today exactly eight calls on this type exist outside engine's tests, all in `compat`; nothing in `crates/app` touches `TimerState` at all. |
| D28 | **No game-specific code.** A behaviour change must implement what `reference/cpp/**` does. A fix that special-cases one title to make it work is forbidden. | We are a compatible emulator, not a port. A title-shaped patch is right for exactly one game and silently wrong for every other — that is what a compatibility hazard *is*. A game trace may **reveal** a bug; only the reference decides the fix, and the fix must be general enough that any title relying on the same reference behaviour benefits. |
| D29 | **Parity retires as a target; the invariant is the VM dispatch surface.** The live dump (`crates/compat/tests/runtime_surface.rs`) is locked to `docs/surface/runtime_surface.json` by `scripts/surface_lock.py` — any added or removed member fails the gate, so L3 can be refactored freely while that dump is unchanged. | Parity measured *members present*, not *behaviour correct*: `Timer` reported 100% while its `capacity`, `interval` type and action-name dispatch differed from the C++. It also could not block invented surface — extras were informational only. So parity stays a **report** ("what is missing?"), the lock is the **constraint** ("what changed?"), and behaviour is constrained by D28 instead. |
| D30 | **Behaviour tests cite the reference; the code does not.** Every VM-behaviour assertion carries a `reference/cpp` citation. Implementations owe no citation and no resemblance to the C++, and performance work — iterators over index loops, `slotmap`, `smallvec`, `itertools`, zero-cost abstractions generally — is **encouraged**, with new workspace dependencies reviewed in the report. | We ship behaviour, not structure: the test is the oracle, the implementation is free. Mirroring C++ line-for-line would forfeit the Rust tools that make it faster while buying no correctness the citation does not already provide. Fences unchanged: surface lock (D29), reference-matching behaviour (D28). |
| D31 | **Comments describe the code as it is.** No design rationale, no decision numbers, no cross-links to docs, and **no before/after narrative** — "what this replaced" and "what it used to do" are history, and history is read with `git`. What stays: how the code works, `// SAFETY:`, non-obvious invariants, and reference citations **in tests**. | 11 468 comment lines in `compat/src` alone, ~30% comment density across the tree, and 108 lines narrating the past — history in comments competes with the code it sits next to and is immediately stale. The migration decisions live here, in docs, where they are reviewable once rather than repeated in every file. |

| D32 | **Only the Android backend may be stubbed.** L3 (`compat`) has no stubs: every member either behaves like the reference or raises because the reference raises. Members whose reference behaviour is a Windows facility (`HMENU` menus, message receivers, MIDI/CD, `DrawDevice.interface`) must be **emulated** (D18), not reported as unavailable. **Approximations are not permitted anywhere** — a wrong result with no error is worse than a raise, because the game keeps running on wrong pixels. | The VM calls L3 directly, so an L3 stub is a behaviour difference the title sees, not an internal deferral. The games are Windows-native and probe Win32 APIs, so "this host lacks that" is a compatibility bug rather than a typed error. Android is the one backend behind L2's contract that no script can observe. |
| D32 | **Only the Android backend may be stubbed.** L3 (`compat`) has no stubs: every member either behaves like the reference or raises because the reference raises. Members whose reference behaviour is a Windows facility (`HMENU` menus, message receivers, MIDI/CD, `DrawDevice.interface`) must be **emulated** (D18), not reported as unavailable. **Script-observable approximations are not permitted** — a wrong member result with no error is worse than a raise, because the game keeps running on wrong values. (Rasterization internals are covered by D34.) | The VM calls L3 directly, so an L3 stub is a behaviour difference the title sees, not an internal deferral. The games are Windows-native and probe Win32 APIs, so "this host lacks that" is a compatibility bug rather than a typed error. Android is the one backend behind L2's contract that no script can observe. |
| D33 | **L1 may change the `engine` signatures it names.** `engine` keeps depending on the L1 *type* crates (`api`, `fs`, `render`) and its public surface may name their types; an L1 type change that flows into an `engine` signature is accepted, not forbidden. The L2 surface is frozen by **review and audit**, not by a tool: the `cargo public-api` snapshot (D21) and the §12.1 "L1 changes must not change any `engine` type" rule are dropped. | The current layering already lets L3 reach L1 types through `engine`, so hiding them behind opaque L2 wrappers buys no real encapsulation while costing a large, churny refactor. Review catches the changes that matter while keeping the API honest and simple. |
| D34 | **Rendering picks the more efficient path when the result looks near-identical; script-observable semantics stay exactly reference.** Pixel rasterization (hatch/linear-gradient fills, GPU composition, transitions) may use an efficient approximation if the visible output is close, but every value a script can *observe* — member getters/setters, return values, raised errors, `typeof`, string/number conversions — must match `reference/cpp/**` exactly. This amends D32: its "no approximations" ban governs script-observable behaviour, not rasterization internals. | The reference itself is CPU-side and exact; matching it bit-for-bit on every raster path is expensive and invisible to scripts, while a wrong member result is a compatibility bug the title sees. Efficiency and fidelity are separated by what the script can read back. |
| D35 | **A directory input mounts every `*.xp3` under it.** When the loader is given a directory and no explicit `--archive` (the CLI `--base-dir`/positional-directory path, and the Android launcher), it recursively discovers `*.xp3` and mounts them, ordered by file name (case-insensitive) then shallower-first (later mounts win), so `patch.xp3` overrides `data.xp3` and a root archive overrides a nested one of the same name. Explicit `--archive` entries suppress the scan. Symbolic-link directories are not followed. | **Documented extension to D15:** the reference has no folder scan, but requiring a mobile user to pick every archive is not ergonomic. This is the one opt-in exception; the explicit-list path (D15) is unchanged and authoritative when used. |

## 4. Document index

| Doc | Contents |
|-----|----------|
| [`history/01-investigation.md`](history/01-investigation.md) | What the project is today: crate graph, coupling map, data flow, measured facts, and the concrete seams that block the target. |
| [`02-target-architecture.md`](02-target-architecture.md) | The four-layer target, crate layout, dependency rules, ownership boundaries, and how the layer system fits. |
| [`03-middle-layer-api.md`](03-middle-layer-api.md) | The stateless-object / explicit-state API: `RenderPipeline` + `RenderScene` tree + `RenderState`, `FileSystem`+`FileState`, `FontSystem`+`Font`, `Media`+`DecoderState`, `AudioDevice`, host traits, DTOs. |
| [`04-renderer-vulkan.md`](04-renderer-vulkan.md) | Bevy → `ash` design: render list, pipelines, blend mapping, offscreen compositing, glyph rasterization, winit/GameActivity surface, what to delete. |
| [`05-media-engine.md`](05-media-engine.md) | Media engine: image/audio/video decode, FFmpeg vs MediaCodec backends, threading, TJS-poll semantics, migration of `tvp-sound`/`video`. |
| [`06-file-system.md`](06-file-system.md) | Stateless bottom fs surface + host/in-memory roots; how `Storage`/`Xp3Archive`/natives move onto it; Android SAF/AssetManager; the C++ `fopen` bypass. |
| [`07-roadmap.md`](07-roadmap.md) | Phased plan, per-phase deliverables/exit criteria, risks, testing strategy, rollback, and open questions. |
| [`08-tjs-vm-and-native-plugins.md`](08-tjs-vm-and-native-plugins.md) | L4 VM host, L3 compatibility surface, the ABI constraint, static native plugins + the null-plugin object, event adaptation, frame drive. |
| [`09-game-loading.md`](09-game-loading.md) | Archive-flag + positional loading (no folder scan), base dir, mount precedence, startup discovery order, dynamic archives. |
| [`10-android-backends.md`](10-android-backends.md) | AAudio + NDK MediaCodec design and official references; the `ndk`-crate strategy for backends we cannot run here. |

## 5. Scope of this branch

In scope (docs only):

* the investigation above and the target design;
* the middle-layer API sketch and the migration sequencing.

Out of scope for this branch:

* any code, crate, dependency or manifest change;
* reversing the decisions above (they are recorded so they can be argued
  about before implementation starts).

## 6. How to read this as a reviewer

1. Read `history/01-investigation.md` to agree on the current-state facts.
2. Read `02-target-architecture.md` and push back on the layer boundaries.
3. Read `03-middle-layer-api.md`; the API is the expensive part to change
   later.
4. Read `07-roadmap.md`; every phase must leave a runnable game, and D9
   (stateless L1) is tested, not just asserted.

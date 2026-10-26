# Architecture rules — layers, state, frame and surface

Status: **authoritative** for the architecture level. This file states the
layer model, the dependency direction, where state lives, who owns the frame,
and what the script can observe. It is the distilled form of the decisions in
§13 below and of the design documents in [`docs/arch/`](../arch/), measured
against the current tree.

Where this file disagrees with the design documents in [`docs/arch/`](../arch/),
those win and this file is corrected. Where it disagrees with
[`AGENTS.md`](../../AGENTS.md), `AGENTS.md` wins.

Related documents:

| topic | document |
|---|---|
| Rust-level checklist | [`code_styles.md`](code_styles.md) |
| public surface, protocol, ownership | [`api_rules.md`](api_rules.md) |
| the architecture behind the rules | [`docs/arch/`](../arch/) |
| per-crate code maps | [`docs/code/`](../code/) |
| the L3 ↔ L2 protocol, in full | [`api_rules.md`](api_rules.md), [`docs/code/compat/engine_api.md`](../code/compat/engine_api.md) |
| the layer tree → frame contract | [`docs/arch/l2.md`](../arch/l2.md) §2–§3, §9 |
| the VM ↔ device pixel mapping | [`docs/arch/l2.md`](../arch/l2.md) §4, [`docs/code/engine/translate.md`](../code/engine/translate.md) |
| the agent contract and the gate | [`AGENTS.md`](../../AGENTS.md) |
| the ordered gate, the tests and the release pipeline | [`gates.md`](gates.md) |

---

## 1. The four layers

```
L4  vm       TJS VM host: the vendored C++ VM, the GC, the bytecode and script
             cache, the frame body
L3  compat   the compatibility surface: native classes, KAG, the event adapter,
             the static plugins, the Win32 emulation
L2  engine   a small, generic game engine: the layer system and scene graph,
             bitmaps, the sound mixer, timers, the VFS namespace; owns state
L1  api · fs · media · psd · render · platform
             stateless services and every platform code: window, event loop,
             input, clock, threads, Vulkan, FFmpeg/MediaCodec, raw IO and
             XP3 parsing
app          the composition root: choose the platform, wire L1..L4, run the
             loop — wiring only, no platform logic
```

| crate | layer | role |
|---|---|---|
| `tjs` | vendored | the C++ TJS2 VM behind an FFI; unchanged, all-FFI |
| `api` | L1 | shared DTOs and the typed `*Error` enums; no implementations |
| `fs` | L1 | the stateless `FileSystem`/`FileState` and the XP3 read/parse |
| `media` | L1 | the stateless `Media` and the image/audio/video decode |
| `psd` | L1 | the stateless PSB container and the PSD image decode |
| `render` | L1 | the stateless `RenderPipeline`/`FontSystem` and Vulkan |
| `platform` | L1 | window/surface, event loop, input, clock, threads |
| `engine` | L2 | the scene graph, the mixer, the timers, the VFS namespace, the projection |
| `compat` | L3 | the native classes, KAG, the event adapter, the plugins |
| `vm` | L4 | the VM host over `tjs` |
| `app` | root | the thin binary that wires the layers |
| `android` | root | the Android composition root (the JNI/GameActivity glue) |
| `testkit` | dev | shared test helpers; dev-dependencies only |

---

## 2. The dependency direction

```
L4 vm      → compat (L3), platform (L1)
L3 compat  → engine (L2), platform (L1), api, fs, tjs
L2 engine  → the L1 type crates: api, fs, render
L1         → the OS, Vulkan, FFmpeg and MediaCodec only
app        → everything, for wiring only
```

There is exactly one extra edge: `L4 → L1`. The VM host uses the platform
abstraction (clock, threads, file access for the bytecode and the state)
without going through the compatibility surface.

### 2.1 What each layer must not contain

* **L1 must not know about layers, TVP, TJS, storages or KAG.** The gate
  rejects `TVP_*`, `LT_*`, `TJS`, `kag`/`KAG` in the source of `api`, `fs`,
  `media`, `render` and `platform`, plus `VK_*` outside `render` (where the
  prefix belongs to Vulkan, not to a virtual-key code).
* **L2 must not contain TVP/TJS vocabulary.** The same tokens, plus `VK_*` and
  the `0..100000` volume scale, are rejected in `engine`. A string literal
  that starts with `file:` is rejected too: the storage-name grammar belongs
  to L3.
* **L3 must not contain platform code.** `#[cfg(target_os = …)]`, `winit`,
  `android-activity`, `std::fs`, `jni` and `ndk` are rejected under
  `crates/compat/src`. L3 reaches the platform through L1 abstractions.
* **L3 names the engine only through `crates/compat/src/engine_api.rs`.** Any
  other file under `crates/compat/src` whose production code names `engine::`
  is a finding. The seam has no allowlist.
* **`#[cfg(target_os)]` and the platform crates live only in L1** (`platform`,
  `media`).

### 2.2 What belongs where — the genericity test

Ask of any behaviour: *does it need the external engine's vocabulary?*

* Generic scene graph, bitmaps, mixing, timers, VFS, projection → **L2**.
* `arc.xp3>path`, masks, province images, `Layer.type`, focus and hit-testing,
  `Font.face`, KAG tags, `.sli`, the `0..100000` volume scale, virtual-key
  codes → **L3**.
* Platform host, window, event loop, raw IO, decode, Vulkan → **L1**.

Two-sided:

* If moving a behaviour into L2 requires naming a TVP concept, it stays in L3.
* If an L3 file grows larger than the L2 module it wraps, the abstraction is
  leaking: push the generic behaviour down, or the shared behaviour struct into
  L2.
* Shared behaviour across several native classes is a **struct composed** by
  thin classes, never a registration list copied per class.

---

## 3. State ownership

### 3.1 L1 is stateless

An L1 object is a facade over a platform capability. Its methods are `&self`,
and every evolving value is an explicit `&mut` argument (`RenderState`,
`FileState`, `DecoderState`, `AudioDecoderState`, …) that L2 owns and stores.

* No `static mut`, `thread_local!`, `lazy_static!`, `LazyLock`, `OnceLock` or
  `OnceCell` in the L1 crates.
* No process-global registry, no hidden cache, no id-keyed table inside L1.
* No `&mut self` method on an L1 service: `RenderPipeline`, `FileSystem` and
  `Media` are immutable after construction.
* No policy in L1: caching, reuse, prefetch, coalescing, deferring and
  throttling are L2 decisions expressed in L2 state.
* No callback from L1 into L2: L1 returns owned data, L2 polls.

The `stateless` gate check enforces this over `crates/platform/{api,fs,media,render,platform}`.

### 3.2 L2 owns the state, L1 owns the traversal

* L2 maintains the `RenderScene` tree and calls `render`. It never builds a
  draw list, never sorts or batches, never selects a blend state.
* L1 performs the traversal, the composition, the batching, the blend
  selection and the drawing, and consumes the L2 tree.
* The projection (`engine::projection`) is a field copy from the logical tree
  to the draw tree, and nothing else. It is testable without a GPU because it
  takes the texture table as a closure.
* The two backends (null/CPU and Vulkan) consume the same draw tree, so their
  composition cannot drift.

### 3.3 The two revisions

The scene carries two counters: a **structure revision** bumped by every change
the projection reads (windows, layers, order, parentage, geometry, draw
properties, bitmap create/remove/resize) and a **content revision** bumped by
every bitmap pixel write. A content write therefore re-uploads the texture
without re-projecting the tree.

---

## 4. The frame contract

**L2 owns the frame.** Nothing above L2 owns a `dt`.

```
Engine::run(clock, hooks) / Engine::frame(now_ms, hooks) / Engine::step(dt_ms, hooks)
  ├─ set the frame clock
  ├─ fit the presentation to the current VM/device sizes
  ├─ deliver the due timers                 (the timer callbacks only post)
  ├─ hooks.before_render(now_ms, dt_ms)     (the one upward hook)
  │      L4 vm.tick → the platform events → the L3 frame body → the audio drain
  ├─ fit the presentation again              (the hook may have resized a window)
  ├─ refresh the render-only overlay
  ├─ upload the dirty bitmaps
  ├─ project the scene when the structure revision changed
  ├─ hooks.after_project(&drawn_layers)     (the self-updating transitions, the
  │                                          script-cached layers)
  ├─ upload again, and project again when the hook changed the scene structure
  └─ one render call
```

* The frame entry point is **public and deterministic**: a test drives
  `frame(now)` with a virtual clock, with no sleeps and no real time.
* `Engine::run(clock, …)` takes the frame's elapsed time from the caller's
  clock source; `Engine::frame(now_ms, …)` sets the clock; `Engine::step(dt_ms, …)`
  accumulates it. The delta handed to the hook is the distance from the
  previous frame's clock, and zero on the first frame.
* The hook is the one upward edge: `GameFrame`, declared in L2 and implemented
  by L3/L4 and wired by the composition root. L2 never names L3 or L4.
* The frame body (`compat::run_frame_body`) **drains the timer posts the engine
  produced**; it must not advance the timer registry. It first requests the
  reference's periodic TJS object rehash (1500 ms), then runs the sound,
  video, async-trigger and continuous-handler polls, the idle compaction and
  the movie/transition tail. Those pumps are still staged inside the L3 hook;
  they keep deterministic test clocks.
* The timer registry's pump is private to L2: `TimerState::advance` is
  crate-private and the engine frame calls it through
  `timer::deliver_due`. A host or a test that owns a registry delivers the
  due timers itself, through the same free function; the compatibility layer
  never advances the registry.
* Re-entrancy: a timer callback mutates L2 state, so a frame collects the due
  work first and then invokes it; the advance is never re-entered.

### 4.1 The timer

The L2 timer surface is exactly four operations:

```
create_interval(interval_ms, callback)
set_interval(id, interval_ms)
set_enabled(id, enabled)
remove(id)
```

L2 waits and calls; L3 decides whether to arm, and turns a fire into an
`onTimer` post. There is no configuration struct, no mode enum and no fire
list handed upward.

### 4.2 The composition root

`app::Host` wraps the engine; `app::GameHost` implements the hook: the VM
tick (input begin → platform events → input end), then the L3 frame body, then
the audio drain. The root selects the platform, builds the services, attaches
the L2 handles to the compatibility host, and runs the loop. It contains **no
platform logic**.

---

## 5. The two coordinate spaces

* **VM pixels** — one pixel of the logical screen (the primary layer). L3 and
  scripts speak them; L2 stores layer rects, bitmap dimensions, font sizes and
  glyph metrics in them.
* **Device pixels** — one pixel of the framebuffer. L1 speaks them.

The mapping is owned by L2 (`engine::translate::PixelTranslation`): a uniform
`min(device/vm)` scale, centred, with the margin cleared. **Fit with letterbox
is the only presentation rule** — there is no mode enum and no scaling
configuration key on any launcher.

* The translation is carried as one transform on the projected render-scene
  root; L1 composes it during traversal. The projected nodes keep VM
  coordinates. At `s == 1` it is the identity, so the reference is matched
  exactly.
* **L1 never interprets the VM semantics.** It receives the VM-space tree and
  the root transform, and writes device pixels; it does not name the
  translation as a VM/device rule.
* **L3 never sees the translation.** An L3 native must not call
  `to_device_*` / `to_vm_point` / `raster_scale`, must not name a device
  pixel, and must not import `PixelTranslation`.
* Input travels the other way: the composition root maps pointer and touch
  coordinates back into VM space before the VM tick, so even an L3 hit-test
  sees VM pixels.
* A script window-size change goes through `Engine::take_window_resize` and
  fits the unchanged screen into the new client. An OS resize changes the
  framebuffer only and is never written back into the scene window.

---

## 6. The error contract

* Where the intended behaviour cannot be performed — an unsupported feature,
  malformed or overflowing data, an unrepresentable size — the operation
  returns a **typed error**. There are no degraded substitutes, no arbitrary
  size caps, no "guess and continue".
* One `#[non_exhaustive]` error enum per domain, owned by the crate that
  declares it. A new failure mode is a new variant, handled at the call site.
* The compatibility layer has **no stubs**: every member either behaves like
  the reference or raises exactly where the reference raises. A wrong member
  result with no error is worse than a raise, because the game keeps running
  on wrong values.
* **Rasterization is the one exception.** Rendering may take a more efficient
  path when the visible output is near-identical (hatch and gradient fills,
  GPU composition, transitions). Every value a script can observe — a member
  getter or setter, a return value, a raised error, `typeof`, a string or
  number conversion — stays exactly reference-exact.
* A compatibility claim names its evidence and its scope. A test is evidence
  for what it asserts and for nothing else; a knowingly-left divergence is
  recorded as an **approximation** with its reference gap, never as
  "unobservable".

---

## 7. The host environment

Games are Windows-native and probe Win32 APIs. `HWND`, draw devices, message
receivers, the registry, the shell and the system metrics must return
**emulated** values and handles — synthetic, stable integers — and never a
typed error. A typed error there is a compatibility bug, not correct
behaviour. The emulation lives in L3; no real Win32 API is linked.

---

## 8. Plugins

* Native plugins are **static and built into L3**: one `BUILTIN_PLUGINS`
  table, and `Plugins.link(name)` recognizes every ported DLL (and its
  aliases).
* A DLL that is **not** ported gets the **null plugin object**: it accepts
  every member access and every call, returns void, logs a warning on every
  call, and never throws. This is the single documented exception to the
  no-silent-fallback rule — and it is not silent.
* The null object never replaces an existing global binding; it is installed
  only where the plugin's stem is free.
* Where the external engine's source is unavailable, the shipped DLL build is
  the oracle and is cited **by hash**
  (`<Name>.dll <sha256[:12]> @ <function/address>`). The extracted surface or
  behaviour is committed as a derived fixture and the test gates on the
  fixture; a test never loads a DLL at run time. The game DLLs themselves are
  not committed to this repository.

---

## 9. Game loading

* Loading is **explicit**: `--archive`, repeatable, later archives win.
* `startup.tjs` is found inside the mounted archives; a missing startup is a
  typed error.
* A positional argument or `--base-dir` roots the relative archive paths and
  the loose files.
* **One opt-in extension**: a directory input with no explicit `--archive`
  mounts every `*.xp3` under it, discovered recursively, ordered by file name
  (case-insensitive) and then shallower-first, with later mounts winning.
  Symbolic-link directories are not followed. An explicit `--archive` list
  suppresses the scan and stays authoritative.

---

## 10. The script-visible surface

* The VM dispatch surface — which classes exist and which members they
  expose — is the contract. `crates/compat/tests/runtime_surface.rs` asks the
  **live VM** what the compatibility host actually registered, and the dump is
  compared with the committed baseline `docs/surface/runtime_surface.json`.
* An added member is invented surface; a removed member is silently broken
  dispatch. Both fail the gate.
* The dump is ground truth from the running VM, not a parse of the source.
* The parity reports (`docs/native_parity.md`, `docs/plugin_parity.md`) are
  **reports**: they say what the reference has that the port does not. They do
  not gate a change, and they never license an invented member.
* The compatibility layer's internals are free to change **while the dump is
  unchanged**. A change to the dump is a reviewed baseline change.

---

## 11. The L3 ↔ L2 seam

L3 implements the script API by composing L2 primitives. It owns no engine
state and does not grow a second engine.

* Every engine item compat touches is re-exported by
  `crates/compat/src/engine_api.rs`, grouped by engine module. The seam is a
  re-export, not a wrapper: a protocol type *is* the engine type.
* **No L1 service appears in the seam.** `FileSystem`, `RenderPipeline`,
  `Media`, the platform services and the renderer are L1; L3 never names one.
* **Every seam item is L2 state or an L2 action on that state.**
* Adding an item: add or reuse it in `engine` under an L2-generic name,
  re-export it in the seam, document it, and use it from L3 through
  `crate::engine_api::…`. Removing one deletes it from the engine, the seam,
  the documentation and every call site in the same change.
* The full protocol — the items per module, their ownership, their coordinate
  space, their error type and their lifetime — is summarised in
  [`api_rules.md`](api_rules.md) and itemised per module in
  [`docs/code/compat/engine_api.md`](../code/compat/engine_api.md).

---

## 12. Platform and backend selection

* The platform is selected at **compile time**: `platform-linux` or
  `platform-android`, exactly one. The media backend follows:
  `video-ffmpeg` on Linux, `video-mediacodec` on Android.
* `#[cfg]` and the backend crates stay inside L1.
* The Android target is never built on the host: the `platform-android` feature
  compiles the platform-independent helpers plus a typed stub, and everything
  that needs the NDK stays gated to `target_os = "android"`. Android-only code
  that has not been compiled is documented as unverified, not described as
  working.
* `krkr-gate android-target` resolves the dependency graph per target and
  fails when a desktop-only backend feature leaks into the Android target, or
  when two mutually exclusive backends are enabled together.

---

## 13. The decisions, distilled

Each row is the rule as it binds the tree today. The reasoning behind these
rules was recorded in a design record that is **not part of this repository**,
so it is not cited here: the pages that state each rule in full are in
[`docs/arch/`](../arch/) and in this file.

| # | the rule as it stands |
|---|---|
| D9 | L1 is stateless: facades over platform capabilities, explicit `&mut` state values, no globals, no `&mut self` on a service |
| D10 | L1 owns the traversal, the composition, the batching and the draw; L2 keeps the tree current and never builds a draw list |
| D11 | No silent fallback and no hardcoded limit: a failure that cannot be performed is a typed error |
| D13 | The platform and the media backend are chosen at compile time, not bundled at run time |
| D14 | Native plugins are static and built into L3; an unported DLL gets the null plugin object, which logs and never throws |
| D15 | Loading takes an explicit archive list; the startup is found inside the mounted archives; a missing startup is a typed error |
| D16 | L2 stays generic and L3 stays thin: an L3 member reduces to marshal → one L2 call → marshal |
| D18 | The Win32 environment is emulated: handles, devices, the registry, the shell and the metrics return emulated values, never errors |
| D26 | L2 owns the frame: the clock, the timer pump, the hook, the projection and the single render call |
| D27 | The L2 timer is `create_interval` / `set_interval` / `set_enabled` / `remove` |
| D28 | No game-specific code: a behaviour change implements what the reference does, for every title |
| D29 | The script dispatch surface is the contract, locked by a baseline diff |
| D30 | The tests cite the reference; the implementation owes no citation and no resemblance to it |
| D31 | A comment describes the code as it is: no history, no rationale, no decision numbers, no cross-links |
| D32 | Only the Android backend may be stubbed; the compatibility layer has no stubs and no script-observable approximations |
| D33 | L1 may change the `engine` signatures it names; the L2 surface is frozen by review and audit, not by a snapshot |
| D34 | Rendering may approximate when the output is near-identical; every script-observable value stays reference-exact |
| D35 | A directory input mounts every `*.xp3` under it, recursively and in a defined order, unless an explicit archive list is given |
| D36 | A compatibility claim names its evidence and its scope; a left divergence is recorded as an approximation |
| D37 | A pointer is never a handle: no sign test, no narrowing, no hidden pointer property; an id crosses script-visible state |
| D38 | Where the reference is a binary, the DLL is the oracle and is cited by hash, and the test gates on a committed fixture |
| D39 | Presentation is one rule: Fit with letterbox. No mode enum, no scaling key |
| D40 | The VM ↔ device translation lives in L2, at the L2 ↔ L3 boundary, and is applied when projecting and reading back |
| D41 | The translation is embedded in L2; L3 never sees it and keeps its VM-coordinate contract |

---

## 14. What the gate enforces

| check | the rule it protects |
|---|---|
| `stateless` | §3.1 — no process-global state and no stateful service in L1 |
| `layering` | §2.1 — layer vocabulary, platform code in L3, the `engine_api` seam, `SAFETY:` notes, the file-level `unsafe` allow, `missing_docs` |
| `error-handling` | §6 and [`code_styles.md`](code_styles.md) §4 — no swallowed error, no pointer sign test, no `#[allow(dead_code)]` |
| `house-style` | §6 — no decision number, no external-engine citation and no history in a production comment |
| `storage-boundary` | §9 — a script storage name is stripped of its media prefix before it crosses into L2 or the host |
| `storage-media` | §9 — every `scheme://` domain the reference registers is served or documented |
| `native-construction` | the native payload carries the state its members need the moment it exists |
| `lifecycle-order` | a native teardown removes or invalidates only the records its own script object owns |
| `callback-objthis` | a member that invokes a caller's callback receives the caller's `objthis` |
| `native-arity` | a native method whose reference form takes zero arguments has a zero-argument branch |
| `void-getter` | a native getter never hands the script a raw `void` where the reference returns a value |
| `host-facts` | every host-fact seam is installed by production code |
| `surface-lock` | §10 — the script-visible surface is the committed baseline |
| `native-parity`, `plugin-parity` | §10 — reports, not gates |
| `android-target` | §12 — no backend-feature leak into the Android target |

# 08 — TJS VM, compatibility surface, and native plugins

Status: design. No code on this branch.
Related: [`02-target-architecture.md`](02-target-architecture.md) (layers),
[`09-game-loading.md`](09-game-loading.md) (loading).

---

## 1. Placement

```
L4  vm       TJS VM host over tjs              (topmost)
L3  compat   native classes + KAG + event adapter + plugins
L2  engine   generic game engine (layer system, audio, VFS)
L1  platform window/host, fs + archive parse, render, media
```

The VM is **not** the host. The event loop is platform code and lives in
**L1**. L4 executes scripts and runs the per-frame body; L3 adapts between
the C++ VM and the L2/L1 engine. Nothing above L1 contains platform
`#[cfg]`.

## 2. L4 — the VM host (`vm`)

Owns:

* the `Tjs2Engine`, the log callback, bytecode compilation and the script
  cache;
* one VM per process, **single-threaded**;
* the per-frame body (the polls, §6).

Uses from L1: the clock, threads, and savedata/state IO — through the L1
platform abstraction, so L4 has no OS-specific code.

Small host API (illustrative):

```rust
impl VmHost {
    fn new(platform: &Platform) -> Result<Self, VmError>;
    fn install(&self, surface: &CompatSurface) -> Result<(), VmError>;
    fn exec(&self, source: &str, name: &str) -> Result<TjsValue, VmError>;
    fn tick(&mut self, frame: FrameCtx<'_>) -> Result<(), VmError>;
}
```

## 3. L3 — the compatibility surface (`compat`)

Sub-parts:

1. **binding glue** — marshalling TJS values ↔ Rust and registering classes
   through the `tjs` `NativeClassBuilder`;
2. **native classes** — the KiriKiri-visible object model (`System`,
   `Storages`, `Scripts`, `Window`/`Layer`/`Bitmap`/`Font`/`Timer`,
   `SoundBuffer`, `VideoOverlay`, `Plugins`, `Debug`, `AsyncTrigger`,
   transitions, menu, …);
3. **KAG** — parser + layer semantics;
4. **event adapter** — §5;
5. **plugins** — §4.

L3 is **thin glue**. TVP behaviours that are a combination of primitives
L2 already exposes are composed in L3 rather than pushed into L2. The
generic **layer system (scene graph) is L2**; L3 adds the TVP semantics
(masks, province images, exact property names, focus/hit-test,
screen-buffer primary layers, storage naming) on top of it.

### 3.1 The ABI constraint — the one allowed global

The C++ ABI hands native callbacks only the raw `tjs2_engine*`; `tjs`
exposes **no user-data slot**. The old `tvp-*` code worked around this with
process-global `LazyLock<Mutex<…>>` contexts. We keep exactly **one**
process-wide binding — an engine → context table in `compat` — because
the ABI forces it. It is L3 (never L1), it holds *dispatch* state (a
reference to the L2 session/engine), never game state, and it is documented
here. **D9 (no globals) still holds for L1.**

## 4. Native plugins

### 4.1 Static, built into L3 (D14)

We build native plugins **into the binary**. (A WASM host was considered and
deferred, §4.3.) A plugin is a named unit:

```rust
pub trait NativePlugin {
    /// The name scripts use: "menu.dll", "windowEx.dll", "fstat.dll", ...
    fn name(&self) -> &str;
    /// Register the plugin's native classes on the VM.
    fn register(&self, ctx: &mut CompatCtx) -> Result<(), CompatError>;
    /// Optional per-frame hook (async triggers, video overlay, timers).
    fn poll(&self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}
```

The built-in set is the TVP core classes plus emulations for the DLL
plugins the shipped games reference (`menu.dll`, `windowEx.dll`,
`fstat.dll`, …). Registration happens at startup; `ctx` carries the L2
services explicitly, so plugins own no hidden state.

### 4.2 Missing DLLs → the null plugin object

Linux and Android cannot load the Windows DLLs the games name. Behaviour of
`Plugins.link(name)`:

* **built-in emulation exists** → activate it;
* **otherwise** → install a **null plugin object** that:
  * accepts *every* member access and call,
  * returns void/undefined and does nothing,
  * logs a `warn!` per call and a `debug!` with the call name and arguments;
* **never throws.**

Rationale: games call `Plugins.link` for plugins we cannot load; throwing
would make the game completely unplayable. This is the **single documented
exception to D11** (no silent fallback) — and it is *not* silent: every
call logs a warning.

### 4.3 WASM (future)

If third-party plugins must actually execute, a WASM host can be added
behind the same `NativePlugin` interface later. Out of scope for the first
milestone.

## 5. Event adaptation (L1 → L3 → L4)

```
platform event   →  L1 platform (winit / android-activity)
                 →  L1 PlatformEvent
                 →  L3 adapter (map to a TJS-visible event)
                 →  L4 VM (dispatch to script handlers)
```

L2 keeps hit-test/focus state; L3 maps raw events to the TJS event surface.
No `#[cfg]` above L1.

## 6. Frame body and poll order

L1 drives the tick; L4 runs the body:

```
timer_poll → sound_poll → video poll
           → async_trigger_poll → continuous_handler_poll
           → L2 projects the scene → L1 RenderPipeline::render
```

## 7. Migration from `tvp-*`

| today | target |
|---|---|
| `tvp-natives` (system/debug/trans/menu/video/plugins) | `compat/plugins/*` |
| `tvp-storages` | `compat/plugins/storages.rs` (over the L2 VFS) |
| `tvp-scripts` | `compat/plugins/scripts.rs` |
| `tvp-input` | `compat` adapter + L1 events |
| `tvp-visual` scene/bitmap | L2 `engine` (generic layer system) |
| `tvp-visual` natives | `compat/plugins/visual/*` |
| `tvp-text` | L1 raster (`render`) + L2/L3 layout/composition |
| `tvp-sound` | L2 model + L1 `media`/audio device |
| `tvp-kagparser` + `kag` | `compat/kag` |

## 8. Testing

* Marshalling and the null-plugin object are unit-testable with no window.
* The VM host is exercised with small script fixtures.
* Native parity (the report script, removed with the legacy crates) keeps the class/method coverage
  tracked through the move.

## 9. Native class structure: share the implementation, register the real surface

The C++ reference gives each class a **distinct** member set (`Layer` 150,
`Window` 83, `Bitmap` 20, `Font` 22, `Timer` 6) even though they share a large
common implementation (the `tTVPBaseBitmap` / native-base node). Our first port
mirrored the *code* but registered the **union** of the visual members on every
class (~144 each) — a 5× source duplication **and** a compatibility deviation
(scripts can call e.g. `Bitmap.changeScreenMode`, which the reference does not
expose). That over-registration is what inflates the parity "extra" column.

Target design (do **not** mirror the C++ classes one-to-one):

* **One shared implementation** for the common node behavior. The L2 `Scene`
  accessors already are that implementation; keep the L3 dispatch functions and
  macros shared rather than copied per class.
* **Register each class's real surface**, per `docs/native_parity.md`, not the
  union. Where two classes genuinely share a member, share the Rust function /
  macro — never the registration list.
* Optionally express the common surface once as a **TJS base class** and
  `extends` it (the ABI already supports `class X extends Y`). The reference does
  not use TJS inheritance for these classes, so prefer per-class registration
  unless a base class proves cleaner.
* Consequence: the parity "extra" column becomes truthful (today ~567 of 613
  extras are this over-registration) and the L3 source shrinks substantially.

This also lets classes that perform the *same action* share one implementation
rather than each re-wrapping the C++ shape — the guiding rule is "model the
script-visible behavior, not the C++ class tree".

### Shared behavior structs (the concrete pattern)

The way to do this is a **shared behavior struct** that each native class
*composes* instead of re-implementing:

* **Audio:** one `Audio` core owns buffering, volume/pan, loop, fades, the
  play/pause/stop state machine and the event queue. That core already exists as
  the L2 `engine::sound::{SoundSource, SoundBuffer, SoundChannel, Mixer}` model.
  `WaveSoundBuffer`, `CDDASoundBuffer`, `MIDISoundBuffer` are then thin L3
  classes that differ only in how the source is obtained (file decode / CD track
  / MIDI sequence — a host seam) and in which members they register. One
  implementation, several thin registrations.
* **Visual nodes:** one node core — the L2 `Scene` + `LayerState`/`BitmapState`
  accessors — is the shared behavior; `Window`/`Layer`/`Bitmap`/`Font`/`Timer`
  are thin classes that register only their own surface and delegate to it.
* **General rule:** put the reusable behavior in the **lowest** layer that can
  own it without TVP specifics (usually L2); the L3 class is a thin adapter that
  registers the script surface and calls the shared struct. When a struct is
  genuinely TVP-specific it still lives **once** in L3 and is composed by the
  classes that need it (e.g. the `Timer`/`AsyncTrigger` registries, the KAG
  parser).

### L3 must stay thin (D16)

L3 is a VM↔engine adapter, **not a second engine**:

* Every native member should reduce to *marshal args → one L2 call → marshal the
  result*. If a member contains real logic (loops, state machines, blending,
  timing, scheduling), that logic belongs in L2 unless it is genuinely
  TVP-specific.
* Express the marshalling **declaratively**. The `layer_int_prop!`-style macros
  (one line per member) are the model; generalizing them across the families so
  a member is a table entry — not a hand-written 40-line trampoline — is the
  single biggest lever on L3's size.
* Audit signal: if an L3 file is larger than the L2 module it wraps, the
  abstraction is leaking upward. Current suspects: `visual/layer.rs` (2.4k),
  `system.rs` (2.2k), `video.rs` (2.2k), `sound.rs` (2.0k).
* **Genericity test (don't over-abstract either).** When moving behavior into
  L2 it must be expressible with **no TVP vocabulary**. If the only way to place
  something in L2 is to name a TVP concept — an `lt*` blend type, a VK key code,
  a `.sli` field, the `0..100000` volume scale, a KAG tag — it stays in L3. L2 is
  a generic engine (layer/scene graph, audio, video, VFS, timers), **not** a TVP
  compatibility layer. A TVP-shaped L2 API is just as wrong as a fat L3.

### Follow-up refactors (D16)

1. **Sound:** move the `.sli` format (`parse_sli`/`SliInfo`/`WaveLabel`/
   `LoopLink`) and the `TVP_*` constants out of `engine::sound`; L2 keeps only a
   generic loop/label model that L3 populates.
2. **Layer:** `LayerState.blend` is the renderer's generic
   `api::blend::BlendMode` (done); the native `lt*` numbering and the
   int↔enum mapping live in L3 (`natives/visual/layer/blend.rs`).
3. **Menu:** do **not** add a menu-specific L2 model. The menu is just another
   use of the engine's existing **generic tree** (the node / parent / children
   structure the layer system already provides); the L3 `MenuItem` tree is built
   on those primitives, with `HMENU`/`keycodeToText` staying in L3. General rule:
   L2 exposes a few *generic* primitives (tree/node, layer, sound, video, VFS,
   timer); L3 composes them — we do **not** add an L2 abstraction per TVP
   concept.
4. **Prune** native members the reference does not expose (in progress).
5. **Declarative marshalling:** generalize the `*_prop!`/`*_method!` macros across
   the families so a member is a table entry, not a hand-written trampoline.
6. **Win32 emulation shim (D18):** the Windows-native members
   (`Window.HWND`/`drawDevice`/`registerMessageReceiver`, the `System` Win32
   members, the message loop) must return **emulated** values/handles — a
   synthetic `HWND`, an emulated draw device, an emulated message receiver — not
   typed errors, so a game that probes Win32 sees a plausible Windows.

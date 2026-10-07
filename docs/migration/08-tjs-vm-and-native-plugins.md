# 08 — TJS VM, compatibility surface, and native plugins

Status: design. No code on this branch.
Related: [`02-target-architecture.md`](02-target-architecture.md) (layers),
[`09-game-loading.md`](09-game-loading.md) (loading).

---

## 1. Placement

```
L4  krkr-vm       TJS VM host over tjs2-sys              (topmost)
L3  krkr-compat   native classes + KAG + event adapter + plugins
L2  krkr-engine   generic game engine (layer system, audio, VFS)
L1  krkr-platform window/host, fs + archive parse, render, media
```

The VM is **not** the host. The event loop is platform code and lives in
**L1**. L4 executes scripts and runs the per-frame body; L3 adapts between
the C++ VM and the L2/L1 engine. Nothing above L1 contains platform
`#[cfg]`.

## 2. L4 — the VM host (`krkr-vm`)

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

## 3. L3 — the compatibility surface (`krkr-compat`)

Sub-parts:

1. **binding glue** — marshalling TJS values ↔ Rust and registering classes
   through the `tjs2-sys` `NativeClassBuilder`;
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

The C++ ABI hands native callbacks only the raw `tjs2_engine*`; `tjs2-sys`
exposes **no user-data slot**. The old `tvp-*` code worked around this with
process-global `LazyLock<Mutex<…>>` contexts. We keep exactly **one**
process-wide binding — an engine → context table in `krkr-compat` — because
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
platform event   →  L1 krkr-platform (winit / android-activity)
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
| `tvp-natives` (system/debug/trans/menu/video/plugins) | `krkr-compat/plugins/*` |
| `tvp-storages` | `krkr-compat/plugins/storages.rs` (over the L2 VFS) |
| `tvp-scripts` | `krkr-compat/plugins/scripts.rs` |
| `tvp-input` | `krkr-compat` adapter + L1 events |
| `tvp-visual` scene/bitmap | L2 `krkr-engine` (generic layer system) |
| `tvp-visual` natives | `krkr-compat/plugins/visual/*` |
| `tvp-text` | L1 raster (`krkr-render`) + L2/L3 layout/composition |
| `tvp-sound` | L2 model + L1 `krkr-media`/audio device |
| `tvp-kagparser` + `kag` | `krkr-compat/kag` |

## 8. Testing

* Marshalling and the null-plugin object are unit-testable with no window.
* The VM host is exercised with small script fixtures.
* Native parity (`scripts/native_parity.py`) keeps the class/method coverage
  tracked through the move.

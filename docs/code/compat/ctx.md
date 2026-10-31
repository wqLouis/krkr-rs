# `ctx.rs` — `CompatCtx` and `CompatServices`

**Source:** `crates/compat/src/ctx.rs`

Two types live here:

* **`CompatServices`** — the L3 dispatch state: the handles to the L2 services
  the natives compose, the per-family state map, and the scratch buffers the
  C ABI needs.
* **`CompatCtx`** — the handle a native registers against and dispatches
  through. It is lifetime-free on purpose: the ABI hands a callback only the
  raw `tjs2_engine*`, so the context is rebuilt from addresses every time it
  is needed.

---

## 1. `CompatServices` — the service handles

```rust
pub struct CompatServices {
    scene:  Option<*mut Scene>,
    storage: Option<*mut Storage>,
    input:  Option<*mut InputState>,
    timer:  Option<*mut TimerState>,
    clock:      Option<*mut FrameClock>,
    frame_time: Option<*const (dyn FrameTimeSource + 'static)>,
    linked_array: RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)>,
    noop_closure: Cell<Tjs2ValueId>,
    states: StateMap,
}
```

### The six handles

| Handle | Attached by | Used by |
|---|---|---|
| `scene: *mut Scene` | `attach_scene` | the visual natives (window, layer, bitmap, font) and the tree owner |
| `storage: *mut Storage` | `attach_storage` | `Storages`, `Scripts`, `WaveSoundBuffer.open`, `Bitmap.load`, the pre-rendered font map |
| `input: *mut InputState` | `attach_input` | `Mouse`, `Key`, the event adapter, the touch getters, `Layer.setCursorPos` |
| `timer: *mut TimerState` | `attach_timer` | the `Timer` class and the frame body's timer drain |
| `clock: *mut FrameClock` | `attach_clock` | the timer registry's pump, `sound_poll`, the transitions, the video and movie steps, the housekeeping intervals |
| `frame_time: *const dyn FrameTimeSource` | `attach_frame_time` | `System.getTickCount`, the script's live tick count |

The handles are **raw pointers**, not borrows, because the process-wide
binding is lifetime-free: the host owns the services for the lifetime of the
VM and the binding only stores addresses.

The accessors therefore hand out the **pointer**, not a `&Scene`. The
container cannot prove the L2 owner is still alive, so forming a shared
reference inside `CompatServices` would be an unsound aliasing hazard.
Callers dereference under their own `unsafe` contract — the documented
invariant is that the attached value outlives the binding and that the
single-threaded VM holds no `&mut` to it during a callback.

Every attach is `unsafe` and says so.

### The scratch buffers

`linked_array` holds the buffers `Plugins.getList` writes its result into. The
C++ trampoline copies the string array immediately after the callback returns,
so the buffers must outlive the call. They live here rather than in a
thread-local so that the **only** process-wide state stays the documented
binding.

### The no-op closure

The null plugin hands a no-op callable back for an absent member read or
call. The closure is created **once** at install time and *duplicated* per
access, so a member read never re-enters the VM and never clobbers the
engine's "last object" slot.

It is stored as a raw `Tjs2ValueId` rather than a `DetachedValue`, and the
Rust handle is deliberately leaked (`mem::forget`): the engine owns the C++
retention and frees it at destruction, so dropping the host can never touch
the engine. `clear_noop_closure` forgets the id without touching the engine,
which makes it safe to call during teardown.

---

## 2. The per-family state map

```rust
type StateCell = RefCell<Box<dyn std::any::Any>>;
type StateMap  = RefCell<HashMap<TypeId, Box<StateCell>>>;
```

`CompatServices::state::<T>()` is the extension point that keeps native-family
state out of process globals. A family defines its own state type, defaults it,
and reads it back:

```rust
let mut state = services.state::<VisualState>();
state.objects.layers.insert(id, handle);
```

The map's borrow is **released before the entry is borrowed**, so two
different families' states can be borrowed at once (nested access). Each entry
is boxed and never removed or replaced, so its address is stable for the whole
`&self` lifetime. Borrowing the **same** `T` twice still panics — that is the
correct behaviour for an aliased `&mut T`.

The families that use it:

| State type | Family | Holds |
|---|---|---|
| `VisualState` | `natives/visual` | the scene-id → script-object registry, the `Window.add` vector, the `Timer` bindings, the host request queues, the string scratch |
| `SystemState` | `natives/system` | the command line, the exit request, the title, the continuous handlers |
| `StringScratch` | several | the reusable `out.string` buffer (one per family, so a re-entrant visual callback cannot clobber a `System` result mid-marshal) |
| `SoundState` | `natives/sound` | the shared mixer, the sound objects, the decoder chain, the vis-buffer registrations |
| `VideoState` | `natives/video` | the overlays, the installed `VideoSource` |
| `AsyncTriggerState` | `natives/async_trigger` | the trigger bindings and the pending fires |
| `MenuState` | `natives/menu_item` | the menu tree nodes |
| `DebugState` | `natives/debug` | the log ring, the log file, the logging handlers, the console/controller singletons |
| `ResourceResolver` | `resource.rs` | the text engine, the face cache, the host image codec, the pre-rendered font maps |
| `TimerPosts` | `natives/visual/timer.rs` | the queued `onTimer` posts and the per-timer cancellation generation |
| `InputDispatchState` | `event.rs` | the mouse capture, the touch captures, the enter/leave tracking |
| `FontMetricsState` | `natives/visual/font.rs` | the per-font `faceIsFileName` flag, the active rasterizer index |
| `HostFacts` | `host.rs` | the virtual-desktop rectangle provider |
| `ClipboardState` | `natives/clipboard` | the installed clipboard backend |

---

## 3. `CompatCtx` — the dispatch handle

```rust
pub struct CompatCtx {
    engine:   usize,
    services: usize,
    _nosend:  PhantomData<*mut ()>,
}
```

It is deliberately **lifetime-free**: the C ABI hands native callbacks only
the raw engine pointer, so the process-wide binding resolves the raw engine
back to this context as plain addresses and the context reconstructs
references on demand.

It is **not** a safe value handle:

* `!Send + !Sync` (the `PhantomData<*mut ()>`) because the VM is
  single-threaded;
* not `Copy`/`Clone`, so it cannot be duplicated into an unbounded lifetime;
* the raw accessors (`engine()`, `services()`, `services_mut()`) are `unsafe`
  — the caller must uphold the "bound and alive" contract. The safe
  registration helpers on this type rely on that invariant.

### The registration helpers

| Method | Registers |
|---|---|
| `register_class(builder)` | a static native class (`System`, `Storages`, `Plugins`, …) |
| `register_instance(builder)` | an instance native class (`Window`, `Layer`, `Bitmap`, `Font`, `Timer`, `AsyncTrigger`, `MenuItem`, `VideoOverlay`, `BitmapLayerTreeOwner`, `KAGParser`, `NullPlugin`, the draw devices) |
| `register_static_members(members)` | static members on an already-registered class |
| `register_instance_members(members)` | instance members on an already-registered class, including a TJS2 builtin such as `Array`/`Dictionary` |
| `delete_member(id, name)` | removes a member from a retained object |

All of them map the `tjs` error string into `CompatError::Register` (or
`CompatError::Engine` for `delete_member`).

### Where the members land

`register_instance` registers the members **on the class** — the reference
`TJS_BEGIN_NATIVE_MEMBERS` shape. Two instance shapes then coexist:

* `new Timer()` — the native class creates the object, and the class's
  `FuncCall` copies every non-static member onto the new object as an own
  property.
* `class OnceTimer extends Timer { function onTimer() {…} }` — a script
  subclass creates the object; object creation registers the **script**
  members as own properties *after* the extends chain is wired, while the
  native members stay reachable through the class chain.

Either way a script-defined member shadows the native default. That is why
every dispatch seam in this crate resolves the member **on the object**
(`Tjs2Engine::call_member` — `Timer.onTimer`, `AsyncTrigger.onFire`,
`MenuItem.onClick`, `Layer.onTransitionCompleted`) instead of invoking the
native callback directly. `crates/compat/tests/extend_dispatch.rs` pins it.

---

## 4. Errors

| Variant | Raised when |
|---|---|
| `CompatError::Register(...)` | a native class or member registration failed |
| `CompatError::Engine(...)` | `delete_member` failed: the member is absent or the id is not an object |

`delete_member` is the removal path for the reference's
`NCB_REGISTER_SUBCLASS_DELAY(X)` shape, whose class is published only for the
duration of the delay.

---

## 5. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/common/mod.rs` | the shared harness attaches the scene, storage, input, timer and frame clock through `services_mut()` before `install` |
| `crates/compat/tests/extend_dispatch.rs` | a script subclass override wins over the native member for every event forwarder |
| `crates/compat/tests/oop.rs::instanceof_walks_the_script_to_native_chain` | the class chain a subclass builds resolves through the native base |
| `crates/compat/tests/dispatch_*.rs` | a `with`-style write from a foreign receiver lands on the **member's** object, not the receiver's |

---

## See also

* [`binding.md`](binding.md) — the table that resolves a raw engine pointer to a `CompatCtx`
* [`host.md`](host.md) — who owns the services
* [`engine_api.md`](engine_api.md) — what the five handles point at
* [`../../arch/l3.md`](../../arch/l3.md) §7 — the one binding and the surface lock

# `vm` — the L4 TJS VM host

Source: `crates/vm/src/lib.rs`, `crates/vm/src/host.rs` · crate `vm` · layer **L4**

`vm` is the topmost layer. It owns the TJS2 engine instance, the log
callback, the script execution surface and the per-frame body. It drives
the L3 compatibility surface and uses the L1 platform abstraction; it never
names L2.

The **VM is not the host**: the event loop is platform code and lives in L1.
L4 executes scripts and runs the frame body; L3 adapts between the C++ VM
and the L2/L1 engine. The layer model is in
[`../arch/l4.md`](../arch/l4.md); the rules that bind this crate are in
[`../rules/arch_rules.md`](../rules/arch_rules.md) §2.

---

## 1. The host

```rust
pub struct VmHost {
    compat: CompatHost,        // the L3 dispatch state — declared first
    adapter: EventAdapter,     // the L3 event adapter
    log_dedup: Box<LogDedup>,  // the per-host log deduplicator
    engine: Box<Tjs2Engine>,   // the C++ engine — declared last
}
```

| field | meaning |
|---|---|
| `compat` | the L3 host: the native surface state and the registry |
| `adapter` | the L3 adapter that turns platform events into script events |
| `log_dedup` | the state the engine's log callback points at; it is boxed so the address stays stable while the host moves |
| `engine` | the C++ engine, boxed so its address is stable for the L3 binding |

### The drop order matters

The fields are declared so the engine is dropped **last**: the dispatch
state retains values whose `Drop` releases an id on the engine, so the
state must be released while the engine is still alive. The `Drop`
implementation therefore:

1. calls `compat.shutdown()`, which releases the native state while the
   engine is alive;
2. flushes the log deduplicator, emitting the final suppression summary
   and capturing any log the shutdown produced;
3. detaches the log callback, so a late engine log cannot reach the state
   that drops with the host.

---

## 2. Construction and the services seam

```rust
impl VmHost {
    pub fn with_default() -> Result<Self, VmError>;
    pub fn new(registry: PluginRegistry) -> Result<Self, VmError>;
    pub fn with_default_services(attach: impl FnOnce(&mut compat::CompatServices)) -> Result<Self, VmError>;
    pub fn new_with_services(registry: PluginRegistry,
                             attach: impl FnOnce(&mut compat::CompatServices)) -> Result<Self, VmError>;
}
```

The L2 service handles — the scene, the storage, the timer registry, the
input state — are **not owned by the VM host**. The composition root
attaches them through `CompatServices` **before** the compatibility surface
is installed, and that order is not cosmetic: a plugin may read a service
while it registers, so the handle must already be there when
`CompatHost::install` runs.

`with_default` and `new` attach nothing.

---

## 3. The public surface

```rust
impl VmHost {
    pub fn engine(&self) -> &Tjs2Engine;
    pub fn compat(&self) -> &CompatHost;
    pub fn compat_mut(&mut self) -> &mut CompatHost;
    pub fn exec(&self, source: &str, name: &str) -> Result<TjsValue, VmError>;
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, VmError>;
    pub fn tick(&mut self, frame: FrameCtx<'_>) -> Result<(), VmError>;
}
```

`exec` runs a script, `eval` evaluates an expression. Both report a
failure as `VmError::Script`, whose message carries the block name and the
line.

`VmError` is the crate's error type:

| variant | meaning |
|---|---|
| `VmError::Vm` | the VM could not be created |
| `VmError::Script` | compilation or execution failed |
| `VmError::Compat` | the compatibility surface failed |

---

## 4. The frame body

```rust
pub struct FrameCtx<'a> {
    pub now_ms: u64,                    // the engine's frame clock
    pub dt_ms: u32,                     // the elapsed time
    pub body: &'a mut dyn FrameBody,    // the ordered polls
    pub events: &'a [PlatformEvent],    // this frame's platform events
}
```

`VmHost::tick` is the whole per-frame body:

1. `begin_input_frame` — clear the per-frame input state;
2. for each platform event: apply it to the L2 input state, then map it to
   a TJS event and dispatch it;
3. `end_input_frame` — commit, so the held and repeat counters advance
   exactly once;
4. `run_frame_body(body, &mut ctx)` — drain the timer posts the engine
   already fired, then the sound, video, async-trigger, continuous-handler
   and frame-tail polls, in that order.

The order is enforced by the L3 `run_frame_body`, not by the implementor of
the polls. Each segment reports its wall-clock cost through the `FrameBody`
hooks, which is how the composition root attributes a slow frame.

The engine owns the clock: the host receives `now_ms` and `dt_ms` from the
engine's frame and never accumulates a delta of its own.

---

## 5. Logging

The engine's log callback is routed through the `log` crate, under the
`tjs:` prefix, with the VM's own level. On top of that the host
**deduplicates exception dumps**.

A dump is a block of console records: a header naming the file, the
function and the VM ip, the disassembly, the register dump, then a
separator. The **identity** of a dump is the header plus the disassembly;
the register dump is excluded, so a repeated failure with different object
addresses still collapses.

* the first three dumps of a run are emitted in full — the first is the
  root-cause evidence, and the next two confirm the repetition is
  deterministic;
* after that the dumps are suppressed, and every hundredth suppressed dump
  produces a summary carrying the running count and the last timestamp;
* a dump with a different identity is always emitted in full, ends the
  previous run and starts a new one — so the first occurrence of every
  distinct failure survives;
* the final summary is emitted when the run ends or the host shuts down.

---

## 6. State and lifetimes

* The host owns no L2 state. Every L2 value it touches is borrowed through
  the services the composition root attached.
* The engine is boxed so the L3 binding's engine→context mapping keeps a
  valid address for the whole life of the host.
* The log deduplicator is boxed for the same reason: the engine's callback
  points at it, and it outlives the engine on every exit path.
* The crate denies `unsafe` at the root; the file that contains the FFI
  allows it locally, and every unsafe block carries a `SAFETY:` note.

---

## 7. Tests

| file | covers |
|---|---|
| `src/host.rs` (unit) | the teardown order (the native state is released while the engine is alive), a caught exception reaching the engine log, the dump deduplication (identical dumps collapse, a different dump is emitted in full and resets the run), and the same behaviour driven through the VM with two distinct failing natives |
| `tests/host.rs` | the public surface: `exec`, `eval` and `tick`, and the order of the polls (`sound`, `video`, `async`, `continuous`, `present`) |

The tests that bind the process-global VM serialize on
`testkit::vm_lock`.

---

## 8. Related documents

| topic | document |
|---|---|
| the vendored C++ VM this host drives | [tjs.md](tjs.md) |
| the L4 layer as a whole | [`../arch/l4.md`](../arch/l4.md) |
| the layer rules and the L3 ↔ L2 seam | [`../rules/arch_rules.md`](../rules/arch_rules.md) §2, §11 |
| the VM, the compatibility surface, the plugins and the frame body | [`../migration/08-tjs-vm-and-native-plugins.md`](../../docs.old/migration/08-tjs-vm-and-native-plugins.md) |
| the composition root that owns this host | [app/game.md](app/game.md), [app/README.md](app/README.md) |
| the shared test helpers | [testkit.md](testkit.md) |

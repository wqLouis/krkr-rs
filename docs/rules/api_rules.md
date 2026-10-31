# API rules — the public surface, the protocol and the ownership

Status: **authoritative** for the API level. This file states what a public
surface is, how it is frozen, how it may change, and who owns what. It is the
distilled form of the surface contract in
[`docs.old/migration/02-target-architecture.md`](../../docs.old/migration/02-target-architecture.md)
§12, the protocol in
[`docs.old/migration/14-l3-l2-protocol.md`](../../docs.old/migration/14-l3-l2-protocol.md),
and the decisions in [`docs.old/migration/README.md`](../../docs.old/migration/README.md) §3,
measured against the current tree.

Where this file disagrees with the design record, the design record wins and
this file is corrected. Where it disagrees with
[`AGENTS.md`](../../AGENTS.md), `AGENTS.md` wins.

Related documents:

| topic | document |
|---|---|
| Rust-level checklist | [`code_styles.md`](code_styles.md) |
| layer, state, frame and surface contract | [`arch_rules.md`](arch_rules.md) |
| the architecture behind the rules | [`docs/arch/`](../arch/) |
| per-crate code maps | [`docs/code/`](../code/) |
| the gate checks and the allowlists | [`scripts/README.md`](../../scripts/README.md) |
| the agent contract | [`AGENTS.md`](../../AGENTS.md) |

---

## 1. There are four surfaces

| surface | where it lives | how it is frozen |
|---|---|---|
| **the script surface** | the classes and members the compatibility host registers on the VM | the committed dump `docs/surface/runtime_surface.json`, checked by `surface-lock` |
| **the L3 ↔ L2 protocol** | `crates/compat/src/engine_api.rs` | the seam module plus the protocol document; the `l3-engine-api` rule enforces that it is the only place compat names the engine |
| **the L2 public API** | the `engine` crate | by review and audit against the layer rules — not by a snapshot |
| **the L1 public API** | `api`, `fs`, `media`, `psd`, `render`, `platform` | by review; an L1 type may flow into an `engine` signature, which is accepted |

A change usually touches more than one. Say which one it touches, and why.

---

## 2. The script-visible surface

### 2.1 What it is

The surface is what a script can reach: the classes the compatibility host
registers, and the members on each of them. `crates/compat/tests/runtime_surface.rs`
asks the **live VM** what `CompatHost::install` actually registered and writes
a deterministic dump; the `surface-lock` check compares that dump with the
committed baseline.

The dump is ground truth from the running VM, not a parse of the Rust source.
That is what makes the lock meaningful: a refactor that cannot change the dump
cannot change how a call reaches a native.

### 2.2 The lock

* An **added** member is invented surface — the hallucinated-API case.
* A **removed** member is silently broken dispatch.
* Both fail the gate. The compatibility layer's internals are free to change
  **while the dump is unchanged**.
* Changing the dump deliberately is a reviewed baseline change: the new
  baseline is committed with the change, so the diff is reviewable. A bare
  `--update` with an unexplained diff is a rejected change.
* `krkr-gate surface-lock --update` is the main agent's command, run after the
  change is understood.

### 2.3 The parity reports are reports, not gates

`docs/native_parity.md` and `docs/plugin_parity.md` say what the reference has
that the port does not. They do not constrain a change, they never license an
invented member, and they are not part of the pass/fail contract. The lock is
the constraint; the reference behaviour constrains the semantics.

### 2.4 Script-visible names

* A member name, a class name, an error string and an enumeration spelling are
  **exactly** the reference's, including case and punctuation.
* A value a script can observe — a getter, a setter, a return value, a raised
  error, `typeof`, a string or number conversion — matches the reference
  exactly. Rendering may approximate internally; the observable value may not.
* A member the reference implements as a zero-argument form keeps that form:
  the native has a zero-argument branch rather than an arity guard that
  rejects it.
* A member that invokes a caller's callback receives the **caller's**
  `objthis`, so a callback that resolves its members against `this` sees the
  caller's receiver.
* A native payload carries the state its members need **the moment it
  exists**: the VM creates the instance before the script's constructor runs,
  so a member touched from a class prelude must already work.
* A native teardown removes or invalidates only the records the **same** script
  object owns. A layer's record belongs to the layer's own script object, so a
  parent layer or a window only detaches it; the layer keeps its native state
  and its finalizer still runs against a live record.
* A property getter returns what the reference returns. A backing
  `StoredValue` field whose constructor default is `StoredValue::Void` is
  mapped to the reference's non-void value — an empty array, a dictionary, the
  class's own object — before it reaches the script. A property that genuinely
  reads back void is recorded in
  `scripts/allowlists/void_getter_allow.txt` with its reason.

### 2.5 The storage-name grammar belongs to L3

A script-visible storage name may carry the local `file` media prefix
(`file://.`, `file://`, `file:`). The L2 namespace takes a bare name within a
mount. Every L3 loader that hands a script name across that boundary routes
it through `crate::storage::strip_file_media` first. A script name that
reaches L2 or the host filesystem unstripped is a finding.

---

## 3. The L2 public API

### 3.1 Explicit state, not hidden state

* Every evolving value is an explicit struct owned by L2: the scene, the
  projection result, the render state, the surface state, the file state, the
  decoder states, the mixer, the timer registry, the input state, the
  transition machines, the caches.
* A state value's fields are `pub` where there is no invariant, and the type
  carries `#[non_exhaustive]` so growth stays additive.
* A service in L1 is immutable: `RenderPipeline`, `FileSystem` and `Media` have
  no `&mut self` method. The evolving value is a separate `&mut` parameter.

### 3.2 Additive-only

* New functions, methods and variants are fine.
* Removing an item, changing its signature, changing its meaning or changing
  an observable value requires an explicit decision entry in the decision
  record, and a reviewed change.
* The L2 surface is frozen by **review and audit**, not by a checked-in
  snapshot. There is no public-API snapshot tool in the gate, and a new one
  is not wanted.

### 3.3 What may not appear in the L2 API

* **TVP vocabulary.** No `LT_*`, no `TVP_*`, no virtual-key codes, no `.sli`,
  no KAG tags, no `0..100000` volume scale. The generic concept is used
  instead: `BlendMode`, a normalized gain, an opaque key code.
* **A cross-layer import.** A layer owns its constants and types; an upper
  layer does not import a lower layer's constant or type. A constant consumed
  by more than one layer means the abstraction has leaked — fix the
  abstraction, not the import.
* **Raw pointers and `usize` addresses.** A handle that crosses the stable
  surface is an opaque newtype or a stable id, never an address. (A script
  property may *report* an address as a number, but the API is not built on
  it.)
* **A process-global.** L2 owns the state; it does not hide it in a static.

### 3.4 An L1 type may flow into an `engine` signature

The engine depends on the L1 type crates (`api`, `fs`, `render`) and its
public API may name their types (`RenderScene`, `FileState`, `FontMetrics`, …).
An L1 type change that flows into an `engine` signature is **accepted**, not
forbidden. What is not accepted is an L1 change that needs a new *engine
concept*: that is an additive L2 change and is reviewed as one.

### 3.5 Documentation

Every public item carries a doc comment, and every crate root carries
`#![warn(missing_docs)]`. The doc states the contract, the units and the
failure mode: `# Errors` for a fallible function, the coordinate space for a
pixel value, the lifetime requirement for an attached handle.

---

## 4. The L3 ↔ L2 protocol

### 4.1 The seam

`crates/compat/src/engine_api.rs` is the only file under `crates/compat/src`
allowed to contain `engine::`. It is a **re-export**, not a wrapper: a protocol
type *is* the engine type, so the host's public signatures, the script surface
and the surface lock are unaffected by the seam's shape.

The seam is grouped by engine module — the scene, the storage namespace, the
input state, the timer registry, the text engine, the mixer, the video
overlay, the transitions, the raster primitives and the layer/bitmap
operations. The short doc comments in the seam are the in-tree summary; the
protocol document is the contract.

### 4.2 Adding an item

1. Add or reuse the item in `engine`, with an L2-generic name and contract.
2. Re-export it in the correct seam group.
3. Document it in the protocol document, with its ownership, coordinate space
   and error type where those are not obvious.
4. Use it from L3 through `crate::engine_api::…`.

### 4.3 Removing an item

Delete it from the engine, from the seam, from the protocol document and from
every call site **in the same change**. No compatibility shim, no stubbed old
signature, no "kept for reference" copy.

### 4.4 What must not enter the seam

* The L1 services (`FileSystem`, `RenderPipeline`, `Media`, `FontSystem`, the
  platform services). L2 calls them internally; L3 never names one.
* The renderer, the draw tree, the projection, the backend.
* The pixel translation itself. `PixelTranslation` and its `to_device_*` /
  `to_vm_point` / `raster_scale` are L2-internal; exposing one to L3 would
  spread the coordinate rule across hundreds of call sites.
* `Engine` itself. The composition root owns it. The **frame
  clock** does cross the seam, as `FrameClock`: the script's tick count is
  the clock the frame advances, so L3 reads the engine's clock instead of
  keeping a second one.
* The platform backends, the game loading and the startup discovery.
* The VM.

### 4.5 What crosses it

* **Coordinates:** VM pixels only. Every rect, bitmap dimension, font size,
  glyph metric, readback coordinate and input position in the protocol is a VM
  pixel. A protocol item that returned or accepted a device coordinate would
  be a compatibility bug.
* **Errors:** typed values owned by the module that declares them. L3 maps
  them to whatever the script-visible behaviour is; it never invents a
  default. `Option` means "absent" and is distinguishable from a typed error;
  `()` means the operation is infallible on already-validated bounds.
* **Lifetime:** the attached handles must outlive the binding. See §7.

---

## 5. Error enums

* One `#[non_exhaustive]` error enum per domain, owned by the crate that
  declares it: `FsError`, `MediaError`, `RenderError`, `GlyphError`,
  `AudioError` (in `api`), `TextError`, `SoundError`, `TimerError`,
  `VideoError`, `TransError`, `StorageReadError`, `MountError`,
  `StartupError`, `ConfigError` (in the layer that owns the domain).
* A new failure mode is a new variant in the owning enum, handled at the call
  site. It is never a default value, a wildcard arm or an L3-side substitute.
* The variant carries the information the caller needs to decide what to do;
  the `Display` text is what the script sees.
* **In L3, an enum that L3 matches variant by variant must not be
  `#[non_exhaustive]`.** A new variant has to be a compile error in L3, not a
  silent arm. The `error-handling` gate reports any `#[non_exhaustive]` under
  `crates/compat/`; the reviewed exceptions are the error enums that are
  converted through their `Display` text rather than matched.
* Errors are values. A panic is not an error path for input a script, a file
  or a device can influence.

---

## 6. Conversions and numeric limits

* `TryFrom` / `try_from` and `checked_*` for narrowing and size arithmetic;
  `From` / `into` for widening.
* An unchecked `as` never truncates a script-supplied or file-supplied value.
  A size that does not fit is a typed error, not a truncated length.
* A value that may carry a pointer or an address is never sign-checked, never
  narrowed and never round-tripped through `NonZero*`. Where the reference
  hands a script an address as an integer, the port mirrors it and treats the
  number as an opaque token; a handle that crosses script-visible state is a
  stable id.
* `Option` for absence, `Result` for failure, and `Result<Option<T>, E>` where
  "not found" and "failed" must be told apart.

---

## 7. Ownership and lifetimes

### 7.1 L1

* The service is immutable and owns no state. The evolving value is a
  separate struct the caller owns and passes in as `&mut`.
* Two services can run in the same process; nothing in L1 is a singleton.

### 7.2 L2

* L2 owns every state value. The scene, the mixer, the timer registry, the
  input state, the transitions and the caches are L2 values.
* L3 may *construct* an L2 state value (a text engine, a mixer, a transition)
  as part of a native payload. The value stays L2-typed and lives in the
  host's dispatch state or in the native payload — never in a second global.

### 7.3 The one process-wide binding

The VM's C ABI hands a native callback only the raw engine pointer and has no
user-data slot, so the compatibility layer keeps exactly **one**
process-wide binding between the engine and the context.

* `CompatServices` holds explicit handles to the L2 services the natives
  compose: `attach_scene`, `attach_storage`, `attach_input`, `attach_timer`,
  `attach_clock`, `attach_frame_time`.
* The attachment is `unsafe`, and the invariant is: **the pointed-to value must
  stay alive for as long as the binding is installed** (the VM's lifetime).
  Detaching and teardown happen before the L2 owner is dropped. A value that
  holds the handle must also keep its **address** stable: the handle is a
  pointer, so moving the value out from under it invalidates every
  attachment. A replacement engine's state objects are bound the same way and
  owe the same obligation.
* The accessors return the **raw pointer**, not a reference. The services
  cannot prove the owner is still alive, so forming a `&Scene` there would be
  an unsound shared reference; the natives dereference under their own
  `unsafe` contract, with a `// SAFETY:` note.
* The scratch buffers and the per-family dispatch state live in the same
  binding, so the documented binding remains the only process-wide state.
* The timer callbacks are removed before the binding is torn down: a callback
  may re-enter the VM and must not run after teardown.
* The compatibility binding is process-global and single: a test drops the
  first environment before creating a second.

### 7.4 The composition root

The root holds the services and the engine and no frame state of its own; it
drives one engine frame per step. It has no globals, no statics and no hidden
caches — every value is a field of the host and is passed to the bottom
objects explicitly.

---

## 8. Module layout and re-exports

* A crate root re-exports **deliberately**: the intended contract, named
  item by item. A flat glob re-export (`pub use crate::…::*;`) is rejected by
  the `house-style` gate, because it makes the public surface accidental.
* An item that is not part of the intended contract stays in its module. The
  primitives, the blend numbering and the constants are reachable through
  their own modules rather than through the crate root.
* An L3-internal item is `pub(crate)`. The seam's re-exports are
  `pub(crate)` too: the protocol is between the two layers, not a public API
  of the compatibility crate.
* The module doc states the module's job, not its past.

---

## 9. Naming the public API

* A script-visible name is the reference's, exactly.
* An L2 name is generic and describes what the code does now: no layer
  numbers, no external-engine vocabulary, no decision numbers, no reference
  file names.
* An accessor that only moves a field does not exist; a public field does
  the job. See [`code_styles.md`](code_styles.md) §3.1.
* A unit belongs in the name or the type.

---

## 10. Changing a public surface

1. **Name the surface.** The script surface, the protocol, the L2 API or the
   L1 API. They have different review paths.
2. **Script surface:** implement the reference behaviour, cite it in the
   test, and commit the new baseline with the change so the diff is
   reviewable. The main agent runs `surface-lock --update` once the change is
   understood.
3. **Protocol:** add or remove in the engine, in the seam, in the protocol
   document and at every call site, in the same change.
4. **L2 API:** an additive change is fine; a removal or a semantic change
   needs a decision entry and a reviewed diff. An L1 type change that flows
   into an `engine` signature is accepted and stated in the change
   description.
5. **Do not** keep the superseded item, the old test, the old allowlist entry
   or the old document section "for compatibility". Deleting them is part of
   the change.
6. **Verify:** `cargo fmt`, then the compiler-free checks of
   [`AGENTS.md`](../../AGENTS.md) §0.4, then the main agent's full gate.

---

## 11. What the gate enforces

| check | the rule it protects |
|---|---|
| `surface-lock` | §2.1–§2.2 — the script surface is the committed dump |
| `layering` | §4.1 — `engine_api.rs` is the only place compat names the engine; the `unsafe` placement and the `missing_docs` lint |
| `error-handling` | §5 — no `#[non_exhaustive]` on a variant-matched L3 enum, no swallowed error, no `#[allow(dead_code)]`, no pointer sign test |
| `house-style` | §8 — no glob re-export, no bean accessor, no decision number in a production comment |
| `stateless` | §7.1 — no process-global state and no `&mut self` on an L1 service |
| `void-getter` | §2.4 — a native getter never hands the script a raw `void` |
| `native-arity` | §2.4 — a zero-argument reference form keeps a zero-argument branch |
| `callback-objthis` | §2.4 — a callback receives the caller's `objthis` |
| `native-construction` | §2.4 — the payload carries the state its members need |
| `lifecycle-order` | §2.4 — a teardown removes or invalidates only its own records |
| `storage-boundary` | §2.5 — a script name is stripped before it crosses the boundary |
| `host-facts` | §7.3 — every host-fact seam is installed by production code |
| `native-parity`, `plugin-parity` | §2.3 — reports, not gates |

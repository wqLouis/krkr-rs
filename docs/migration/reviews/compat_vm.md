# L3/L4 review — `krkr-compat` (compatibility surface) and `krkr-vm` (VM host)

Scope: `crates/krkr-compat/` (plugin architecture, null plugin, `CompatCtx`/
binding, event adapter, frame body), `crates/krkr-vm/` (VM host), and the
recently added `tjs2_prop_set_default` ABI (`crates/tjs2-sys/cpp/tjs2_abi.{h,cpp}`,
`crates/tjs2-sys/src/lib.rs`). Read against
`docs/migration/02-target-architecture.md` (§1–§3, §6–§9),
`docs/migration/03-middle-layer-api.md` and
`docs/migration/08-tjs-vm-and-native-plugins.md` (§3–§6, D14).

Method: source read-only. Only `cargo check` was run, as permitted:

* `cargo check -p krkr-vm --all-targets` — clean, no warnings.

No `build`/`run`/`test` was executed.

Severity legend:

| Severity | Meaning |
|----------|---------|
| **Critical** | UB/use-after-free/null-deref reachable from a safe public call. |
| **High** | Layer/D9/D14 violation with functional consequences (wrong script-visible value, broken lifecycle, unbounded re-entrancy/global state). |
| **Medium** | Correctness edge cases, lifetime/aliasing hazard behind a safe seam, missing coverage of a new ABI path. |
| **Low** | Local idiom/robustness/consistency; worth fixing opportunistically. |

Overall the design matches doc 08: the single process-wide engine→context
binding is small and documented, the null plugin correctly uses the VM
`missing` handler, and `run_frame_body` enforces the documented poll order.
The main problems are (a) `CompatCtx` is an unsound safe handle, (b) the null
plugin recompiles/handles a closure per member access, (c) `unlink` is not a
real unlink and breaks `link→unlink→link`, and (d) L3 redeclares
`PlatformEvent` instead of consuming L1's. `tjs2_prop_set_default` itself is
correct (verified against `tjs2_prop_set`).

---

## 1. `CompatCtx` is a safe, `Copy + Send + Sync`, lifetime-free handle — Critical

`crates/krkr-compat/src/ctx.rs:77-78` defines
`pub struct CompatCtx { engine: usize, services: usize }` with
`#[derive(Clone, Copy)]`, and `crates/krkr-compat/src/ctx.rs:95,102,113`
expose **safe** `engine()`, `services()` and `unsafe services_mut()` that
dereference those addresses. `crates/krkr-compat/src/host.rs:77-79`
`CompatHost::ctx()` is a safe `pub fn` that returns one of these even when the
host is not installed (`host.rs:42-43` initialise `engine: 0,
services_ptr: 0`).

Two concrete UB paths from safe code:

1. `CompatHost::new().ctx().engine()` → `&*(0 as *const Tjs2Engine)` (null
   deref) with no `unsafe`.
2. `let ctx = host.ctx(); std::thread::spawn(move || ctx.engine().eval(..))`
   compiles because `usize` is `Send + Sync`; the VM is explicitly
   single-threaded (`tjs2-sys/src/lib.rs:432-437`), so this corrupts the C++
   VM. The same handle also outlives the engine (nothing ties its lifetime to
   the binding), making use-after-free possible.

Why it matters: this is the one ABI-forced global, and the whole point of
`binding.rs` is to make it sound; the safe wrapper around it undoes that.

Fix: (a) make `CompatHost::ctx()` return `Option<CompatCtx>`/`Result` (None
until `bound`); (b) add `PhantomData<*mut ()>` (or `PhantomData<Rc<()>>`) to
`CompatCtx` so it is `!Send + !Sync`; (c) either make `engine()`/`services()`
`unsafe fn` or tie the handle to the host with a lifetime
(`CompatCtx<'a>`), which also removes `Copy`. The doc comment at
`host.rs:74-76` acknowledges the handle "is only valid once an engine has
been bound", but a safe fn cannot rely on a caller honouring prose.

---

## 2. The null plugin recompiles/re-evaluates a closure on every member access — High

`crates/krkr-compat/src/null_plugin.rs:149-167` (`ignore_absent_member`)
calls `host.eval_retained("(function(){})", "null_plugin")` for **every**
absent member read and every absent method call, then retains the result and
sets it as the property default. `null_missing` invokes it on each access.

Why it is wrong: it re-enters the VM (`tjs2_eval`) from inside a native
callback on the hot path. Besides the per-access eval/retain/PropSet cost,
`tjs2_eval` does `e->last_object.Clear()` and then records the new closure
(`tjs2_abi.cpp:1400-1415`), so it clobbers the engine's "last object" slot
while the outer native call is still running — a latent hazard for any
callback that returns a handle-less object. The comment claims a fresh
closure is required because `CallGetMissing` creates a temporary property
object; that justifies re-*retaining* a value, not re-*evaluating* one.

Fix: create the no-op function once (e.g. when `NullPlugin` is registered, or
on first use) and cache its retained id/handle in L3 state (`CompatServices`
or the `NullPlugin` payload); on each access duplicate the cached id with
`tjs2_retain_retained_id` (already exposed in `tjs2-sys`) and pass that as
the `TjsValue::Retained` value. The duplicate is what `value_to_variant`
consumes, so the cache stays live.

---

## 3. `PluginRegistry::unlink` is not an unlink and breaks `link → unlink → link` — High

`crates/krkr-compat/src/plugin.rs:130-137` only removes the normalized name
from the `linked` set. It does not unregister the built-in's native classes
and does not remove the null object's script global. Because `activate`
(`plugin.rs:107-128`) re-runs `plugin.register(ctx)` whenever the name is not
in `linked`, a subsequent `link` of the same built-in calls
`register_native_class` again. `tjs2_register_native_class_ex` rejects
duplicate class names (`tjs2_abi.cpp:1711-1714`, returns `-2`), so
`link→unlink→link` fails for every built-in; for the reference,
`TVPUnloadPlugin` allows reload.

Fix: track `link` outcome per name (built-in vs null, plus "already
registered") and have `activate` skip `register` when the classes are already
on the VM; on `unlink`, either remove the null global (the null object is only
reachable through it) or document that built-ins stay registered and make
re-`link` idempotent. Also note the reference `unlink` returns whether the
plugin was unloaded; `set_int(out, 1)` (`plugins_native.rs:139-151`) always
reports success.

---

## 4. `thread_local! ARRAY_OUT` is a second hidden global — High

`crates/krkr-compat/src/plugins_native.rs:18-24` defines a process-wide
(per-thread) `ARRAY_OUT` scratch used by `Plugins.getList`. Doc 08 §3.1 says
there is exactly **one** process-wide binding and that it "holds dispatch
state, never game state"; the review constraint repeats "no hidden globals
beyond the one documented binding". This is a second one, and it is not
mentioned next to the binding. (It is also invisible to
`scripts/check-stateless.sh`, which guards only L1 crates.)

Fix: move the scratch into L3 dispatch state (`CompatCtx`/`CompatServices`, or
the plugin registry) so it is owned explicitly, or — if the ABI truly forces a
static — fold it into the documented binding and say so. Do not leave it
undocumented beside `binding.rs`.

---

## 5. L3 redeclares `PlatformEvent` instead of consuming L1's — High

`crates/krkr-compat/src/event.rs:17` defines a fresh `pub enum PlatformEvent`
(`KeyDown`/`PointerMove`/…), even though L1 already owns
`krkr_platform::PlatformEvent` (`crates/krkr-platform/src/event.rs:13`) and
`krkr_platform::InputEvent` (`crates/krkr-platform/src/input.rs:33`). Doc 02
§3 and doc 08 §5 are explicit: the event loop/type is L1 and L3 only *maps*
it. `VmHost::tick` (`crates/krkr-vm/src/host.rs:116-124`) consumes the L3
type, and `krkr-vm` even depends on `krkr-platform` (`Cargo.toml:11`)
without using it. The two enums already disagree (L1's `InputEvent::Key`
carries an opaque `u32`; L3's `KeyDown` carries an `i64` + string).

Fix: delete `krkr_compat::PlatformEvent`; have `EventAdapter::map` accept
`krkr_platform::PlatformEvent` (and/or `InputEvent`) and produce `TjsEvent`,
and have `VmHost::tick` take the L1 type. Keep the TJS mapping in L3; keep
the platform enum in L1.

---

## 6. Null plugin's script global loses the requested case — Medium

`plugin.rs:123` passes the **normalized** key to
`null_plugin::install(ctx, &key)` (`normalize_plugin_name` lowercases).
`null_plugin::install` uses it for both the global name and the recorded
plugin name (`null_plugin.rs:200-208`). The module doc promises
`Plugins.link("windowEx.dll") → global.windowEx` (`null_plugin.rs:22-23`),
and the reference preserves the caller's name; the actual object is
`global.windowex` and the name logged/returned is lowercased.

Fix: key `linked`/built-in lookup on the normalized name, but pass the
original `name` (or a stem derived from it) to `null_plugin::install`. Add a
test with mixed case.

---

## 7. `CompatServices` hands out `&Scene`/`&Storage` from raw pointers via a safe seam — Medium

`crates/krkr-compat/src/ctx.rs:20-30` stores `Option<*mut Scene>` /
`Option<*mut Storage>`, attached with `unsafe fn attach_scene/attach_storage`,
and `ctx.rs:52-64` return `Option<&Scene>` / `Option<&Storage>`. Nothing ties
that shared borrow to the L2 owner, so L2 can legitimately mutate the scene
(`&mut Scene`) while a native still holds the `&Scene` produced by the safe
`scene()`, causing aliasing UB. The `scene()`/`storage()` accessors are safe
fns, so this is reachable without `unsafe`.

Fix: return the raw pointer (`*mut Scene`) and force callers into `unsafe`, or
hand back a borrow tied to a `&CompatCtx<'a>` whose lifetime is anchored to
the host. Avoid materialising a `&` from a detached `*mut` in a safe fn.

---

## 8. `tjs2_prop_set_default` is correct, but untested and its failure semantics are undocumented — Medium

I checked the new ABI function line by line. It mirrors `tjs2_prop_set`
correctly: it validates the target before converting the value, re-resolves
`id` afterwards, converts the value with `value_to_variant` (which consumes a
`TJS2_VAL_RETAINED` entry, `tjs2_abi.cpp:2417-2470`), uses a NULL member name
for the default member, and distinguishes `eTJS` / `std::exception` / `...`
exactly like `tjs2_prop_set`. `tTJSSimpleGetSetProperty::PropSet` accepts the
NULL name (`tjsObject.cpp:278-294`) and `TJSThrowFrom_tjs_error(hr, nullptr)`
is fine (the name defaults to `nullptr`, `tjsError.h:224`). The no-op-closure
round trip is leak-free and double-free-free: the retention is consumed by
the copy into `var` and the Rust `DetachedValue::drop` release of the erased
id is a documented no-op (`tjs2_abi.cpp:2119-2125`).

What is missing: there is no test for `set_default_member` /
`tjs2_prop_set_default` anywhere (`grep` finds only the definition and the
FFI binding), and the safe wrapper `Tjs2Engine::set_default_member`
(`tjs2-sys/src/lib.rs:1420-1434`) does not document that a
`TjsValue::Retained` value is consumed **even if `PropSet` fails after
conversion** (the entry is erased before the `PropSet` call). A caller that
reuses the id after an error would find it gone.

Fix: add a `tjs2-sys` test that sets a default member and reads it back,
including the error path; document the consume-on-conversion-failure
contract on `set_default_member` (and `set_member`, which has the same
behaviour).

---

## 9. `getList` ordering and case differ from the reference — Medium

`PluginRegistry.linked` is a `BTreeSet<String>` (`plugin.rs:47`), so
`linked()` (`plugin.rs:93-99`) and therefore `Plugins.getList()` return names
in lexicographic order and lowercased, whereas the predecessor
(`crates/tvp-natives/src/plugins.rs:88-92`) preserved insertion order and the
original string. Games that display or index the list can observe the change.

Fix: keep a `Vec` (insertion order, original name) plus a separate
normalized set for membership/link checks.

---

## 10. `run_frame_body` omits the projection/render tail of the documented body — Medium

`crates/krkr-compat/src/frame.rs:50-56` enforces only the five polls. Doc 08
§6 lists the body as `timer → sound → video → async_trigger →
continuous_handler → L2 projects the scene → L1 RenderPipeline::render`.
Because `FrameBody`/`run_frame_body` is described as *the* enforcement point,
and neither projection nor render appears anywhere in the trait, the seam
silently drops the last two steps and `VmHost::tick` returns after the
polls (`krkr-vm/src/host.rs:116-124`). When the visual natives land, this
ordering will be enforced by convention rather than by the function that
claims to enforce it.

Fix: add a final `project_and_render` (or `present`) step to the trait/order
(delegating to L2/L1), or update doc 08 to say the body ends at the polls and
the host drives the render after `run_frame_body`.

---

## 11. Dead error variants and unused dependencies — Low

`CompatError::Engine` and `CompatError::Unsupported` (`krkr-compat/src/lib.rs:60-68`)
are never constructed. `krkr-compat/Cargo.toml:10-13` depends on `krkr-fs`,
`krkr-render` and `krkr-media` (and `krkr-api`), and `krkr-vm/Cargo.toml:10-12`
depends on `krkr-api` and `krkr-platform`, but no source file references any
of them (`grep` is empty). Declaring L2/L1 dependencies the code does not use
weakens the dependency-direction signal.

Fix: drop the unused deps and error variants, or use them (e.g. the L1
`PlatformEvent` per finding 5).

---

## 12. Minor marshalling nits — Low

* `native_util::args` (`krkr-compat/src/native_util.rs:18-24`) returns
  `&'a [Value]` with a caller-chosen lifetime decoupled from the input, which
  lets a caller store a slice past the callback; the lifetime should be tied
  to the borrow.
* `value_to_ffi` narrows the retained id `u64` to `usize`
  (`tjs2-sys/src/lib.rs:1684-1695`); harmless on 64-bit, truncating on 32-bit
  (Android armeabi-v7a).
* `tjs_string_literal` (`null_plugin.rs:218-233`) escapes `\ ' \n \r` but not
  other control characters (tab, NUL), so an unusual plugin name could break
  the generated expression.

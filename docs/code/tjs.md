# `tjs` — the vendored C++ TJS2 VM

Source: `crates/tjs/` · crate `tjs` · the compatibility anchor

`tjs` is the upstream C++ TJS2 scripting VM, compiled by `build.rs` and
exposed through a thin, hand-written Rust wrapper over a C ABI. It is the
only legacy crate the architecture still links, and it is the reason the
whole tree is a **compatible emulator**: the script language, its semantics
and its error behaviour are the reference's, not a re-implementation.

The crate is not a layer. It sits under L4 (`vm`), which owns the engine
instance and drives it; see [vm.md](vm.md) and
[`../arch/l4.md`](../arch/l4.md) §1.

---

## 1. Layout

| piece | where |
|---|---|
| the vendored C++ sources | `crates/tjs/cpp/tjs2/` — 35 `.cpp` and 38 `.h` |
| the three bison grammars | `crates/tjs/cpp/tjs2/bison/` (`tjs.y`, `tjsdate.y`, `tjspp.y`) |
| the C ABI shim | `crates/tjs/cpp/tjs2_abi.cpp` and `tjs2_abi.h` |
| the port-authored C++ | `crates/tjs/cpp/streams.cpp` — the TJS stream IO |
| the Rust binding | `crates/tjs/src/lib.rs` — the `tjs2_*` `extern "C"` surface and the safe wrapper |
| the build script | `crates/tjs/build.rs` |

The vendored tree is read-only for the port: it is the authority on what
TJS means, and the behaviour it defines is the behaviour the compatibility
layer must reproduce.

---

## 2. The build

`build.rs` compiles the VM without cmake:

1. `zig build` in `../../deps` fetches the header-only `fmt` / `spdlog` /
   `boost` and the **oniguruma** regex library, staging them into
   `$OUT_DIR/zig-out`;
2. the three bison grammars are parsed into `$OUT_DIR/gen`;
3. `python3 script/create_world_map.py` generates the date word map;
4. the vendored sources, the generated parsers and the two shims are
   compiled (`-std=c++17 -fPIC`) into objects, archived into
   `libtjs2_core.a`, and linked with `libonig.a` and the C++ runtime.

The toolchain comes from the environment, in the conventional Cargo forms
`<VAR>_<target>`, `TARGET_<VAR>`, `<VAR>`: `CC` (the C compiler, which also
switches oniguruma off the zig path so zig never has to provide a libc for
a cross target), `CXX` (the C++ compiler, default `zig c++`) and `AR`.
`TJS2_OPT` overrides the optimisation level (default `-O2`) and
`TJS2_EXTRA_CXXFLAGS` appends flags for A/B benchmarking; flags that change
float semantics must not be used, because script-observable values are part
of the compatibility surface.

After the compile, the produced objects are checked against the Cargo
target's ELF architecture. A compiler that defaulted to the build host is
rejected with an actionable error instead of silently producing host
objects for a foreign target.

---

## 3. The ABI

The ABI is defined in `cpp/tjs2_abi.h` and implemented in
`cpp/tjs2_abi.cpp`. It is small and stable, and it is mirrored by hand on
the Rust side — no bindgen.

| group | entry points |
|---|---|
| lifecycle | `tjs2_create`, `tjs2_destroy`, `tjs2_set_data_dir`, `tjs2_wire_stream_factories` |
| execution | `tjs2_exec_script`, `tjs2_eval`, and their UTF-16 (`_wide`) counterparts, `tjs2_load_bytecode`, `tjs2_compile_script` |
| logging | `tjs2_set_log_cb` with the `LOG_DEBUG`/`LOG_INFO`/`LOG_WARN`/`LOG_ERROR` levels |
| storage | `tjs2_set_storage_read_callback` — the process-wide read hook the text and binary stream factories use |
| natives | `tjs2_register_native_class`, `…_class_ex`, `…_class_instance`, `…_native_static_members`, `…_native_instance_members` |
| values | `tjs2_retain_value`, `tjs2_release_value`, `tjs2_invalidate_retained`, `tjs2_retain_object`, `tjs2_retain_retained_id`, `tjs2_find_retained_id` |
| members | `tjs2_prop_get`, `tjs2_prop_set`, `tjs2_prop_delete`, `tjs2_prop_set_default`, `tjs2_call_value`, `tjs2_call_value_with_objthis`, `tjs2_call_member`, `tjs2_set_arg`, `tjs2_result_needed` |
| introspection | `tjs2_get_class_names`, `tjs2_enum_members`, `tjs2_set_call_missing`, `tjs2_get_native_instance`, `tjs2_get_stack_trace_string`, `tjs2_dump`, `tjs2_do_gc` |

**The ABI constraint that shapes L3:** a native callback receives only the
raw `tjs2_engine*`, and the ABI exposes **no user-data slot**. That is why
the compatibility layer keeps exactly one process-wide engine→context
binding — see [`../rules/arch_rules.md`](../rules/arch_rules.md) §8.

---

## 4. The Rust wrapper

```rust
pub struct Tjs2Engine { /* the engine + the retained-id registry */ }

impl Tjs2Engine {
    pub fn new() -> Result<Self, &'static str>;
    pub fn exec_script(&self, script: &str, name: &str) -> Result<TjsValue, TjsError>;
    pub fn exec_script_discard(&self, script: &str, name: &str) -> Result<(), TjsError>;
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, TjsError>;
    pub fn eval_retained(&self, expression: &str, name: &str) -> Result<RetainedValue, TjsError>;
    pub fn exec_script_retained(&self, script: &str, name: &str) -> Result<RetainedValue, TjsError>;
    pub fn register_native_class(&self, builder: &NativeClassBuilder) -> Result<(), String>;
    pub fn register_native_class_instance(&self, builder: &NativeInstanceBuilder) -> Result<(), String>;
    pub fn register_native_static_members(&self, members: &NativeStaticMembers) -> Result<(), String>;
    pub fn register_native_instance_members(&self, members: &NativeInstanceMembers) -> Result<(), String>;
    // … retained values, member access, bytecode, GC, introspection
}
```

The four native builders cover the four shapes a script-visible member can
take: a **static** class member, a **per-instance** member with a native
payload, a **static** member attached to a class that already exists, and
an **objthis-only** member attached to an existing class (including a TJS2
builtin such as `Array` or `Dictionary`).

### The value types

| type | meaning |
|---|---|
| `TjsValue` | a value that crossed the ABI directly: `Void`, `Integer`, `Real`, `String`, `Object` (no handle), `Retained` |
| `RetainedValue` | the result of a retained execution: a scalar, or an object retained on the engine as a `DetachedValue` |
| `ValueId<'a>` | a retained id that **borrows** the engine, so a value id can never outlive its engine |
| `DetachedValue` | a retained id detached from the engine borrow; the engine must outlive every `DetachedValue` it created |
| `Tjs2ValueId` | the opaque per-engine id the C side uses |

An object or function result cannot be represented by `TjsValue` — no
object handle crosses the boundary — so it is retained on the engine and
returned as a `DetachedValue`. Dropping a `DetachedValue` releases the
value; passing it to the C side as `VAL_RETAINED` consumes the retention.

### The retained-value registry

The engine keeps the ids of every value retained on it and releases them all
when it is dropped, so a forgotten id (for example through
`std::mem::forget`) cannot leak a retained value or crash the engine drop.

---

## 5. State, lifetimes and threading

* **The engine is single-threaded.** The TJS2 VM keeps process-global state
  (the script cache, string interning), so two engines must never run
  concurrently. The crate's `Send`/`Sync` impls are sound only because the
  tree confines all VM use to one thread; they are a documented gap.
* **The storage read callback is process-wide**, like the stream factories
  themselves: it must be installed before the scripts run and cleared before
  its user pointer dies.
* **Every `unsafe` block carries a `SAFETY:` note**, and the crate is
  `deny(unsafe_code)` except where the FFI genuinely lives.

---

## 6. Failure modes

| failure | how it surfaces |
|---|---|
| a script compile or runtime error | `TjsError`, whose message carries the block name and the line (`<block>(<line>): <message>`) |
| a native that fails | the C++ side allocates an error message with `tjs2_malloc` and the wrapper converts it into a `TjsError` |
| a duplicate class registration | `Err(String)` from the registration call |
| an unknown retained id, a released id, an invalidated object | `Err(String)` from the member call |
| a missing member | a typed error — the reference's own status, not a substituted value |

The error text is the script's own: the VM formats a caught exception with
the failing block and line, so a host-side failure names the script rather
than only the raw message.

---

## 7. Tests

The crate carries **76 unit tests** inside `src/lib.rs` and **10 integration
test files** under `crates/tjs/tests/`.

The unit tests cover the engine lifecycle, the value marshalling, the
interior-NUL representation limit, the native registration and the error
paths of every native shape (a method that fails, a property setter that
fails, an instance method called on the wrong object), the retention
semantics (a retained function called across calls, a released value, the
engine's retained count, a discarded result that does not pin its object),
the static and objthis member shapes, the `compileScript` and bytecode
paths, the GC, the class-name and member enumeration, and the stream
fixtures.

The integration tests pin the areas that needed their own file: member
deletion and its absent-member status, identifier resolution inside a
function that lives in a dictionary literal, member enumeration, the frame
reuse of a retained object, the instance-member shapes, the invalidation of
a retained object, the native instance lookup, the non-BMP string round
trip, the default member, and the save-struct stream.

Every test in this crate serializes on a mutex: the VM's process-global
state makes concurrent engines unsafe.

---

## 8. Known gaps

* **`unsafe impl Send + Sync for Tjs2Engine` is unsound as a public API** —
  two engines would race the process-global VM state — and nothing requires
  the bounds.
* **The TJS stream IO bypasses Rust.** `cpp/streams.cpp` implements the
  `Array.save` / `load` / `saveStruct` paths with `std::fopen` directly.
  It is the one IO path the stateless file-system surface cannot reach.
* **The Rust port of the VM is deferred**, so the build keeps the
  zig / bison / python / oniguruma dependency set.
* `docs.old/vm-defects.md` records the VM and reference defects a rewrite must
  not rediscover.

---

## 9. Related documents

| topic | document |
|---|---|
| the L4 host that drives this crate | [vm.md](vm.md) |
| the L4 layer as a whole | [`../arch/l4.md`](../arch/l4.md) |
| the layer rules and the ABI constraint | [`../rules/arch_rules.md`](../rules/arch_rules.md) §2, §8 |
| the VM, the compatibility surface and the plugins | [`../migration/08-tjs-vm-and-native-plugins.md`](../../docs.old/migration/08-tjs-vm-and-native-plugins.md) |
| the TJS2 language and the Rust port audit | [`../arch/l4.md`](../arch/l4.md) §4–§5 |
| the VM defect record | [`../vm-defects.md`](../../docs.old/vm-defects.md) |
| the evidence trail for the VM decisions | [`../../docs.old/migration/08-tjs-vm-and-native-plugins.md`](../../docs.old/migration/08-tjs-vm-and-native-plugins.md) |

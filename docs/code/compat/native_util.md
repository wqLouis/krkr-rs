# `native_util.rs` — the shared marshalling helpers

**Source:** `crates/compat/src/native_util.rs`

The C ABI hands a native callback an `argc`/`argv` pair, an out slot and an
error slot. These helpers convert the common cases so a raw callback does not
repeat the pointer code.

Every function here is `pub(crate)`; the module is an implementation detail
of the native families, not a public API.

---

## 1. Reading the callback arguments

| Helper | Contract |
|---|---|
| `args(argv, argc) -> &[Value]` | **unsafe**. Views the callback's arguments as a slice. Returns an empty slice for a null pointer or a non-positive count (the C++ side passes a null `argv` when a method is called with no arguments). The returned lifetime is chosen by the caller, so a safe wrapper cannot enforce it. |
| `callback_args(argv, argc) -> &'static [Value]` | The safe form. The slice is only valid while the callback runs; callers must not let it escape the call. |
| `value_ref(value) -> &Value` | The single `Value` a property setter received. The property-setter trampoline passes one valid, non-null `Value` and keeps it alive for the call. |

The `unsafe`/`safe` split is deliberate: `args` cannot express the borrow of
the caller's frame, so the safe wrapper documents the call-scoped lifetime
instead of pretending it is longer.

---

## 2. The native payload

| Helper | Contract |
|---|---|
| `inst_mut<T>(instance) -> Option<&mut T>` | Recovers the mutable native payload a callback received. `None` for a null instance pointer, which the trampoline only passes when the object has no payload. |
| `inst_ref<T>(instance) -> Option<&T>` | The shared form. |
| `host_ref<T>(ptr) -> &'static T` | Borrows an L2 object the host attached for the binding's lifetime. The host keeps the attached scene and storage alive while the binding is installed, and the single-threaded VM holds no `&mut` to them during a callback. |
| `services(ctx) -> &CompatServices` | The L2 services bound to the current callback. A `CompatCtx` exists only while the binding is installed and is used inside the callback that resolved it. |

---

## 3. Coercion

`value_as_string(v) -> String` implements the TJS `ttstr` coercion:

| Value | Result |
|---|---|
| string | the string itself (`to_string_lossy`) |
| integer | its decimal form |
| real | its decimal form |
| void, object, everything else | the empty string |

A null `v.string` on a string value also yields the empty string.

---

## 4. Writing the out slot

| Helper | Writes |
|---|---|
| `set_void(out)` | TJS `void` |
| `set_int(out, value)` | a TJS integer |
| `set_bool(out, value)` | a TJS integer — `true` is `1`, `false` is `0` |
| `set_real(out, value)` | a TJS real (defined in `visual/common.rs`, the visual family's variant) |
| `set_retained_out(host, out, id)` | a retained script object (in `visual/common.rs`) |
| `set_object_out(host, out, object)` | a duplicated retained object, or TJS `null` (in `visual/common.rs`) |
| `set_null_out(host, out)` | TJS `null` (in `visual/common.rs`) |

Each writes the whole `Value` struct — the type tag plus every field — so a
stale field from a previous value cannot leak into the result. A null `out`
slot is ignored: the trampoline always passes a valid one, and a null check
costs nothing.

---

## 5. Raising

| Helper | Behaviour |
|---|---|
| `report_error(out_error, msg) -> c_int` | Writes the message into the callback's error slot and returns the non-zero status code. The C++ trampoline frees the message with `tjs2_free_string`. |
| `alloc_error_string(msg) -> *mut c_char` | Builds a malloc-compatible NUL-terminated UTF-8 copy with `tjs2_malloc`. A failed allocation returns a null pointer and the error is still reported. |

The message text is script-observable, so it is quoted verbatim in each
module's page and pinned by the tests.

---

## 6. Built-in class statics

`register_builtin_static_members(engine, class_name, methods, properties)`
attaches `TJS_STATICMEMBER`-style members to a TJS2 **builtin** class object
such as `Dictionary`.

The reference declares these with `TJS_STATICMEMBER`; the members live on the
class object, are reachable through it, and are **not** copied onto instances,
so an instance's `EnumMembers` never reports them. The C++ entry point is
`tjs2_register_native_builtin_static_members` in `cpp/tjs2_abi.cpp`: it reuses
the objthis dispatch so the callbacks receive the receiver the member was
invoked on, but marks the symbol static.

A class or member name that contains a NUL byte is rejected with a typed
error rather than being truncated.

---

## 7. The test lock

`test_vm_lock()` (compiled only under `cfg(test)`) serialises unit tests that
create a TJS VM. The `tjs` engine keeps process-wide C++ globals and
`compat` installs a single process-wide engine → context binding, so two
`#[cfg(test)]` modules that create an engine concurrently race. Integration
tests use the per-binary `common::vm_lock`; this is the in-crate counterpart.
It is a no-op outside `cfg(test)`.

---

## 8. Tests

The helpers are covered by the families that use them. The lock is covered
by the whole suite — without it the crate's own unit tests would be flaky.

| Test | What it asserts |
|---|---|
| `crates/compat/tests/common/mod.rs::vm_lock` | the integration-side lock |
| `crates/compat/src/natives/visual/mod.rs::timer_poll_fires_the_callback` | an in-crate test that takes the lock |
| `crates/compat/tests/reference_surface.rs` | the reference `ttstr`/integer coercion shapes the helpers implement |

---

## See also

* [`binding.md`](binding.md) — the table that makes `services()` sound
* [`ctx.md`](ctx.md) — `CompatServices`
* [`../../rules/code_styles.md`](../../rules/code_styles.md) §5 — the `unsafe` rules these helpers live under

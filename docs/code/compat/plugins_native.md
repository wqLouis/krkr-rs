# `plugins_native.rs` — the `Plugins` native class

**Source:** `crates/compat/src/plugins_native.rs`

`Plugins` is the script-facing plugin-linking class. It has **static members
only** — no instance, no payload — and mirrors the reference `Plugins` class
with the null-plugin behaviour wired in.

```
Plugins.link(name)    → void
Plugins.unlink(name)  → bool
Plugins.getList()     → array of the linked names
```

---

## 1. `Plugins.link(name)`

Activates a plugin name:

| Outcome | Behaviour |
|---|---|
| a built-in emulation exists | its classes are registered (once) and the link is recorded |
| no built-in emulation | the [null plugin object](null_plugin.md) is installed under the plugin's stem, or an existing global at that stem is kept |

The call returns **void** in both cases — exactly as the reference does. The
outcome is reported through the log:

| Level | Message |
|---|---|
| `info` | `Plugins.link({name:?}): built-in emulation activated` |
| `warn` | `Plugins.link({name:?}): no built-in emulation; null plugin path (doc 08 §4.2)` |

The null path logs its own warning inside `null_plugin::install` as well, so
the "kept an existing global" case is visible too.

`link` requires **one argument**: `"Plugins.link requires 1 argument"`.

---

## 2. `Plugins.unlink(name)`

Forgets a linked name and reports whether it had been linked:

* the built-in classes **stay** on the VM — the ABI cannot remove them; only
  the `getList` entry goes, and a later `link` is idempotent;
* a null plugin's script global is cleared **best-effort**, and only when
  this link installed it and it still holds the null object.

The result is logged at info: `Plugins.unlink({name:?}) -> {removed}`.

`unlink` requires **one argument**: `"Plugins.unlink requires 1 argument"`.

---

## 3. `Plugins.getList()`

Returns the linked plugin names:

* in **insertion order**;
* with the **script's original casing** — the name the script passed, not the
  normalised key.

The reference `getList` has no `numparams` check, so any arguments are
ignored.

### The array marshalling

The C++ trampoline copies the string array **immediately after the callback
returns**, so the buffers must outlive the call. They live in the L3
`CompatServices` dispatch state — `linked_array_scratch` — rather than in a
thread-local, so the only process-wide state stays the documented binding.

The scratch holds a `Vec<Vec<u8>>` of NUL-terminated copies and a
`Vec<*const c_char>` of pointers into them. The out slot receives
`VAL_ARRAY` with the pointer array and its count; the trampoline copies the
strings before the scratch is reused.

---

## 4. Dispatch

All three members resolve the compat context through the process-wide
binding:

```rust
let Some(resolved) = binding::resolve(engine as *mut Engine) else {
    return report_error(out_error, "Plugins.<member>: compat context is not bound");
};
```

`getList` binds the context **before** borrowing the registry, so the scratch
buffers are owned by the same services the callback was resolved against.

---

## 5. Errors

| Message | Raised when |
|---|---|
| `"Plugins.link requires 1 argument"` | `link` was called with no argument |
| `"Plugins.unlink requires 1 argument"` | `unlink` was called with no argument |
| `"Plugins.link: compat context is not bound"` | the binding is not installed |
| `"Plugins.unlink: compat context is not bound"` | the binding is not installed |
| `"Plugins.getList: compat context is not bound"` | the binding is not installed |
| `err.to_string()` | the registry returned an error (a failed registration or a failed null-plugin install) |

---

## 6. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/plugin.rs` | `link` on a built-in activates it; `link` on an unported name takes the null path; `unlink` reports the outcome; `getList` preserves the order and the casing |
| `crates/compat/tests/plugin_autoload.rs` | the autoload path links through the same registry |
| `crates/compat/tests/null_plugin.rs` | the null global the link installs |
| `crates/compat/tests/menu_dll_surface.rs` | `menu.dll` links as a built-in **without** a null global |

---

## See also

* [`plugin.md`](plugin.md) — the registry behind `link`
* [`null_plugin.md`](null_plugin.md) — the object an unported DLL resolves to
* [`binding.md`](binding.md) — how the callback finds its context
